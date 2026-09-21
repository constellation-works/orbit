use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use orbit_types::workflow::{
    JobRunState, JobTargetType, KnowledgeRunMetrics, PipelineState, RunIdRole, run_id_role,
};
use tempfile::TempDir;

use super::super::SqliteJobRunStore;
use crate::Store;
use crate::contracts::{
    ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams, JobRunStepParams, JobRunStoreBackend,
};

#[test]
fn job_run_lifecycle_round_trips() {
    let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
    let scheduled_at = Utc::now();
    let run = backend
        .insert_job_run("job-a", 1, scheduled_at, None, None)
        .expect("insert");
    assert_eq!(run.state, JobRunState::Pending);

    assert!(
        backend
            .mark_job_run_running(&run.run_id, scheduled_at, 42)
            .expect("running")
            .owns_execution()
    );
    let step_params = JobRunStepParams {
        step_index: 0,
        target_type: JobTargetType::Activity,
        target_id: "activity-a".to_string(),
        started_at: scheduled_at,
        finished_at: scheduled_at,
        duration_ms: Some(7),
        exit_code: Some(0),
        agent_response_json: Some(serde_json::json!({"ok": true})),
        state: JobRunState::Success,
        error_code: None,
        error_message: None,
    };
    assert!(
        backend
            .complete_job_run_step(&run.run_id, &step_params)
            .expect("step")
    );
    assert!(
        backend
            .finalize_job_run(&run.run_id, JobRunState::Success, scheduled_at, Some(7))
            .expect("finalize")
    );
    let loaded = backend
        .get_job_run(&run.run_id)
        .expect("get")
        .expect("some");
    assert_eq!(loaded.state, JobRunState::Success);
    assert_eq!(loaded.steps.len(), 1);
}

/// [ORB-10070] A pending run accepts an owner claim (pid recorded); once
/// the run leaves `pending` the claim is refused without writing.
#[test]
fn claim_pending_job_run_owner_only_claims_pending_runs() {
    let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
    let scheduled_at = Utc::now();
    let run = backend
        .insert_job_run("job-claim", 1, scheduled_at, None, None)
        .expect("insert");
    assert!(run.pid.is_none());

    assert!(
        backend
            .claim_pending_job_run_owner(&run.run_id, 4242)
            .expect("claim pending")
    );
    let claimed = backend
        .get_job_run(&run.run_id)
        .expect("get")
        .expect("some");
    assert_eq!(claimed.state, JobRunState::Pending);
    assert_eq!(claimed.pid, Some(4242));

    assert!(
        backend
            .mark_job_run_running(&run.run_id, scheduled_at, 4242)
            .expect("running")
            .owns_execution()
    );
    assert!(
        !backend
            .claim_pending_job_run_owner(&run.run_id, 9999)
            .expect("claim running is refused")
    );
    let running = backend
        .get_job_run(&run.run_id)
        .expect("get")
        .expect("some");
    assert_eq!(running.pid, Some(4242));

    assert!(
        !backend
            .claim_pending_job_run_owner("jrun-missing", 4242)
            .expect("claim missing run is refused")
    );
}

#[test]
fn update_run_serializes_concurrent_mutations_without_torn_write() {
    let temp = TempDir::new().expect("tempdir");
    let db_path = temp.path().join("orbit.db");
    let backend_a = SqliteJobRunStore::new(Store::open(&db_path).expect("store a"), "ws_a");
    let backend_b = SqliteJobRunStore::new(Store::open(&db_path).expect("store b"), "ws_a");
    let scheduled_at = Utc::now();
    let run = backend_a
        .insert_job_run("job-a", 1, scheduled_at, None, None)
        .expect("insert");
    let run_id = run.run_id.clone();
    let barrier = Arc::new(Barrier::new(2));

    let run_id_a = run_id.clone();
    let barrier_a = Arc::clone(&barrier);
    let writer_a = thread::spawn(move || {
        backend_a.update_run(&run_id_a, |run| {
            run.resolved_crew = Some("crew-a".to_string());
            barrier_a.wait();
            thread::sleep(Duration::from_millis(100));
            Ok(())
        })
    });

    barrier.wait();
    let run_id_b = run_id.clone();
    let writer_b = thread::spawn(move || {
        backend_b.update_run(&run_id_b, |run| {
            run.knowledge_metrics = Some(KnowledgeRunMetrics {
                raw_read_token_baseline: 100,
                knowledge_pack_tokens: Some(50),
                compression_ratio: Some(2.0),
                actual_fs_read_tokens_during_run: 25,
                double_read_rate: Some(0.0),
                knowledge_pack_used: true,
                knowledge_pack_unresolved_count: 0,
                total_llm_input_tokens: 75,
            });
            Ok(())
        })
    });

    assert!(writer_a.join().expect("writer a").expect("update a"));
    assert!(writer_b.join().expect("writer b").expect("update b"));

    let loaded = SqliteJobRunStore::new(Store::open(&db_path).expect("store c"), "ws_a")
        .get_job_run(&run_id)
        .expect("read")
        .expect("run");
    assert_eq!(loaded.resolved_crew.as_deref(), Some("crew-a"));
    assert!(loaded.knowledge_metrics.is_some());
}

/// [ORB-12111] Two direct submissions a second apart land in the same minute
/// stem, and so does the first one's own child dispatch. A bare sequence
/// number made the sibling and the child read identically, so a run listing of
/// the three looked like one run tree when it is two. Each id now names the
/// role it was minted for.
#[test]
fn same_minute_siblings_and_children_get_role_marked_ids() {
    let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
    let submitted_at = Utc::now();

    let first = backend
        .insert_job_run("task_ship_pipeline", 1, submitted_at, None, None)
        .expect("first submission");
    let second = backend
        .insert_job_run("task_ship_pipeline", 1, submitted_at, None, None)
        .expect("second submission in the same minute");

    assert_ne!(first.run_id, second.run_id);
    assert_eq!(run_id_role(&first.run_id), Some(RunIdRole::TopLevel));
    assert_eq!(run_id_role(&second.run_id), Some(RunIdRole::TopLevel));

    let parent_state = PipelineState::new(
        first.run_id.clone(),
        first.job_id.clone(),
        serde_json::json!({}),
    );
    backend
        .write_run_state(&first.run_id, &parent_state)
        .expect("seed parent state");
    let child = match backend
        .admit_child_job_run(&ChildJobRunAdmissionParams {
            parent_run_id: first.run_id.clone(),
            parent_step_id: Some("leaf_invoke".to_string()),
            job_id: "task_gate_pipeline".to_string(),
            action: "invoke_detached".to_string(),
            blocking: false,
            attempt: 1,
            scheduled_at: submitted_at,
            input: None,
            authority: None,
        })
        .expect("admit child")
    {
        ChildJobRunAdmissionOutcome::Admitted(child) => *child,
        other => panic!("parent was admitting, got {other:?}"),
    };

    assert_eq!(run_id_role(&child.run_id), Some(RunIdRole::Child));
    assert_ne!(child.run_id, second.run_id);

    // The id's claim and the durable lineage agree: the child belongs to the
    // first run, and the second top-level run is nobody's child.
    let linked = backend
        .read_run_state(&first.run_id)
        .expect("read parent state")
        .expect("parent state")
        .child_dispatches
        .iter()
        .map(|dispatch| dispatch.child_run_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(linked, vec![child.run_id.clone()]);
    assert!(!linked.contains(&second.run_id));
}

#[test]
fn execution_location_is_trusted_immutable_and_legacy_unknown() {
    use orbit_types::task::ExecutionLocation;
    let store = Store::open_in_memory().expect("store");
    let plain = SqliteJobRunStore::new(store.clone(), "ws_a");
    let forged = serde_json::json!({"executed_on":{"machine_id":"payload-machine"},"machine_name":"payload-host"});
    let legacy = plain
        .insert_job_run("job", 1, Utc::now(), Some(forged.clone()), None)
        .expect("legacy");
    assert!(legacy.executed_on.is_none());
    let location = ExecutionLocation {
        machine_id: "registry-machine".into(),
        machine_name: Some("display-name".into()),
    };
    let trusted = plain.with_execution_location(Some(location.clone()));
    let mut run = trusted
        .insert_job_run("job", 1, Utc::now(), Some(forged), None)
        .expect("insert");
    assert_eq!(run.executed_on, Some(location.clone()));
    run.executed_on = Some(ExecutionLocation {
        machine_id: "replacement".into(),
        machine_name: None,
    });
    store
        .upsert_job_run_for_workspace("ws_a", &run, None)
        .expect("ordinary upsert");
    assert_eq!(
        trusted
            .get_job_run(&run.run_id)
            .expect("read")
            .expect("run")
            .executed_on,
        Some(location)
    );
    assert!(
        trusted
            .get_job_run(&legacy.run_id)
            .expect("read legacy")
            .expect("legacy")
            .executed_on
            .is_none()
    );
    assert_eq!(trusted.list_job_runs("job").expect("list").len(), 2);
}

fn pull_fixture() -> (
    TempDir,
    SqliteJobRunStore,
    crate::contracts::PullDestination,
    crate::contracts::AdmissionRequest,
) {
    use crate::contracts::{
        AdmissionRequest, AdmissionRunContext, AdmissionShipContract, PullDestination,
    };
    let temp = TempDir::new().expect("temp");
    let store = SqliteJobRunStore::new(
        Store::open(&temp.path().join("pull.db")).expect("store"),
        "ws",
    );
    let parent = store
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .expect("parent");
    store
        .write_run_state(
            &parent.run_id,
            &orbit_types::workflow::PipelineState::new(
                parent.run_id.clone(),
                parent.job_id,
                serde_json::json!({}),
            ),
        )
        .expect("state");
    let destination = PullDestination {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "ws".into(),
        selector: "owner/ws".into(),
        execution_machine_id: "owner".into(),
    };
    let request = AdmissionRequest {
        request_id: "one".into(),
        caller_version: "1".into(),
        caller_schema: 1,
        caller_review_policy: "none".into(),
        run_context: AdmissionRunContext {
            run_id: parent.run_id,
            job_name: "workspace_auto_pipeline".into(),
            machine_name: None,
        },
        ship: AdmissionShipContract {
            mode: "local".into(),
            base_branch: "main".into(),
            landing_branch: "main".into(),
            review_policy: "none".into(),
            completion: "review".into(),
            authorization_reference: None,
        },
    };
    (temp, store, destination, request)
}

fn pull_receipt(
    request: &crate::contracts::AdmissionRequest,
) -> crate::contracts::AdmissionReceipt {
    use crate::contracts::*;
    AdmissionReceipt {
        schema_version: 1,
        request: request.clone(),
        machine_id: "owner".into(),
        claim: Some(ExecutionClaim {
            claim_id: "claim".into(),
            task_id: "task".into(),
            request_id: request.request_id.clone(),
            executed_on: ExecutionLocation {
                machine_id: "owner".into(),
                machine_name: None,
            },
            run_context: request.run_context.clone(),
            footprint: vec!["file:src.rs".into()],
            reservation_id: "reservation".into(),
            reservation_expires_at: "later".into(),
            phase: ExecutionClaimPhase::Claimed,
        }),
        task: Some(AdmissionTaskSummary {
            id: "task".into(),
            title: "task".into(),
            complexity: None,
            crew: None,
            context_files: vec!["file:src.rs".into()],
        }),
        invalid_candidates: vec![],
        deferred_conflicts: vec![],
        queue_depth: 0,
    }
}

#[test]
fn local_pull_crash_cuts_preserve_one_leaf_and_launch_uncertainty() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::local_pull_crash_cuts_preserve_one_leaf_and_launch_uncertainty",
    ) {
        return;
    }
    use crate::contracts::{LocalPullMutation as M, LocalPullPhase as P};
    let (temp, store, destination, request) = pull_fixture();
    store
        .allocate_pull_request(&destination, &request, 1)
        .expect("allocate")
        .expect("slot");
    let receipt = pull_receipt(&request);
    let mut leaf = None;
    for mutation in [
        M::Receive(Box::new(receipt.clone())),
        M::CreateLeaf,
        M::Bound,
    ] {
        let before = store
            .mutate_local_pull(&destination, "one", &mutation)
            .expect("transition");
        // Drop and reopen the database after each committed cut, replaying the
        // same operation as a caller that lost its response.
        let reopened = SqliteJobRunStore::new(
            Store::open(&temp.path().join("pull.db")).expect("reopen"),
            "ws",
        );
        let after = reopened
            .mutate_local_pull(&destination, "one", &mutation)
            .expect("replay");
        assert_eq!(before, after);
        leaf = after.leaf_run_id;
    }
    assert!(leaf.is_some());
    assert_eq!(
        store
            .list_job_runs("task_claimed_local_pipeline")
            .expect("runs")
            .len(),
        1
    );
    let mut second = request.clone();
    second.request_id = "two".into();
    assert!(
        store
            .allocate_pull_request(&destination, &second, 1)
            .expect("capacity")
            .is_none()
    );
    assert_eq!(
        store
            .mutate_local_pull(&destination, "one", &M::LaunchIntent)
            .expect("intent")
            .phase,
        P::Launching
    );
    assert!(
        store
            .mutate_local_pull(&destination, "one", &M::LaunchIntent)
            .expect_err("uncertain")
            .to_string()
            .contains("deliberate recovery")
    );
    store
        .mutate_local_pull(&destination, "one", &M::Launched)
        .expect("launched");
    assert!(
        store
            .mutate_local_pull(&destination, "one", &M::LaunchIntent)
            .is_err()
    );
}

#[test]
fn local_pull_pending_and_terminal_unsettled_each_hold_one_slot() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::local_pull_pending_and_terminal_unsettled_each_hold_one_slot",
    ) {
        return;
    }
    use crate::contracts::{ClaimEvidence, ClaimMutation, LocalPullMutation as M};
    let (_temp, store, destination, request) = pull_fixture();
    store
        .allocate_pull_request(&destination, &request, 1)
        .expect("allocate");
    let mut next = request.clone();
    next.request_id = "next".into();
    assert!(
        store
            .allocate_pull_request(&destination, &next, 1)
            .expect("pending capacity")
            .is_none()
    );
    store
        .mutate_local_pull(
            &destination,
            "one",
            &M::Receive(Box::new(pull_receipt(&request))),
        )
        .expect("receipt");
    let record = store
        .mutate_local_pull(&destination, "one", &M::CreateLeaf)
        .expect("leaf");
    store
        .finalize_job_run(
            record.leaf_run_id.as_deref().expect("id"),
            orbit_types::workflow::JobRunState::Cancelled,
            Utc::now(),
            None,
        )
        .expect("cancel");
    assert!(
        store
            .allocate_pull_request(&destination, &next, 1)
            .expect("terminal capacity")
            .is_none()
    );
    let fail = M::Settle(Box::new(ClaimMutation::Fail(ClaimEvidence {
        summary: Some("cancelled".into()),
        ..Default::default()
    })));
    store
        .mutate_local_pull(&destination, "one", &fail)
        .expect("durable failure");
    assert!(
        store
            .allocate_pull_request(&destination, &next, 1)
            .expect("settlement capacity")
            .is_none()
    );
    store
        .mutate_local_pull(&destination, "one", &M::Settled)
        .expect("acknowledged");
    assert!(
        store
            .allocate_pull_request(&destination, &next, 1)
            .expect("free")
            .is_some()
    );
    assert_eq!(
        store
            .mutate_local_pull(&destination, "one", &M::CreateLeaf)
            .expect("permanent binding")
            .leaf_run_id,
        record.leaf_run_id
    );
}

#[test]
fn local_pull_launch_intent_refuses_a_cancelled_bound_leaf() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::local_pull_launch_intent_refuses_a_cancelled_bound_leaf",
    ) {
        return;
    }
    use crate::contracts::{LocalPullMutation as M, LocalPullPhase};
    let (_temp, store, destination, request) = pull_fixture();
    store
        .allocate_pull_request(&destination, &request, 1)
        .expect("allocate")
        .expect("slot");
    for mutation in [
        M::Receive(Box::new(pull_receipt(&request))),
        M::CreateLeaf,
        M::Bound,
    ] {
        store
            .mutate_local_pull(&destination, "one", &mutation)
            .expect("prepare");
    }
    let record = store.local_pull_admissions().expect("record").remove(0);
    let leaf = record.leaf_run_id.expect("leaf");
    store
        .finalize_job_run(&leaf, JobRunState::Cancelled, Utc::now(), None)
        .expect("cancel");
    let error = store
        .mutate_local_pull(&destination, "one", &M::LaunchIntent)
        .expect_err("must not launch");
    assert!(error.to_string().contains("no longer queued"), "{error}");
    assert_eq!(
        store.local_pull_admissions().expect("record")[0].phase,
        LocalPullPhase::Bound
    );
    assert_eq!(
        store.get_job_run(&leaf).expect("read").expect("leaf").state,
        JobRunState::Cancelled
    );
}

#[test]
fn local_pull_capacity_replaces_wrapper_with_queued_descendant() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::local_pull_capacity_replaces_wrapper_with_queued_descendant",
    ) {
        return;
    }
    use orbit_types::workflow::ChildDispatch;
    let (_temp, store, destination, request) = pull_fixture();
    let wrapper = store
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("wrapper");
    let gate = store
        .insert_job_run("task_gate_pipeline", 1, Utc::now(), None, None)
        .expect("gate");
    let leaf = store
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .expect("leaf");
    for (parent, child) in [(&wrapper, &gate), (&gate, &leaf)] {
        let mut state = PipelineState::new(
            parent.run_id.clone(),
            parent.job_id.clone(),
            serde_json::json!({}),
        );
        state.child_dispatches.push(ChildDispatch::submitted(
            child.run_id.clone(),
            child.job_id.clone(),
            "invoke_and_wait".into(),
            true,
            true,
            Utc::now(),
        ));
        store.write_run_state(&parent.run_id, &state).expect("link");
    }
    // Wrapper + gate + actual queued leaf occupy exactly one global slot.
    assert!(
        store
            .allocate_pull_request(&destination, &request, 1)
            .expect("full")
            .is_none()
    );
    assert!(
        store
            .allocate_pull_request(&destination, &request, 2)
            .expect("one free")
            .is_some()
    );
    let mut next = request.clone();
    next.request_id = "another".into();
    assert!(
        store
            .allocate_pull_request(&destination, &next, 2)
            .expect("pending uses second slot")
            .is_none()
    );
    store
        .mark_job_run_running(&leaf.run_id, Utc::now(), std::process::id())
        .expect("start leaf");
    store
        .finalize_job_run(&leaf.run_id, JobRunState::Success, Utc::now(), None)
        .expect("finish leaf");
    // The still-live wrapper resumes representing its slot after the leaf ends.
    assert!(
        store
            .allocate_pull_request(&destination, &next, 2)
            .expect("wrapper remains")
            .is_none()
    );
    store
        .mark_job_run_running(&wrapper.run_id, Utc::now(), std::process::id())
        .expect("start wrapper");
    store
        .finalize_job_run(&wrapper.run_id, JobRunState::Success, Utc::now(), None)
        .expect("finish wrapper");
    assert!(
        store
            .allocate_pull_request(&destination, &next, 2)
            .expect("slot released")
            .is_some()
    );
}

/// Link `parent -> child` the way a dispatching pipeline records it.
fn link_dispatch(
    store: &SqliteJobRunStore,
    parent: &orbit_types::workflow::JobRun,
    child_run_id: &str,
    child_job_id: &str,
) {
    use orbit_types::workflow::ChildDispatch;
    let mut state = store
        .read_run_state(&parent.run_id)
        .expect("read parent state")
        .unwrap_or_else(|| {
            PipelineState::new(
                parent.run_id.clone(),
                parent.job_id.clone(),
                serde_json::json!({}),
            )
        });
    state.child_dispatches.push(ChildDispatch::submitted(
        child_run_id.to_string(),
        child_job_id.to_string(),
        "invoke_and_wait".into(),
        true,
        true,
        Utc::now(),
    ));
    store
        .write_run_state(&parent.run_id, &state)
        .expect("link dispatch");
}

/// Take one admission all the way to a queued, bound leaf.
fn admit_queued_leaf(
    store: &SqliteJobRunStore,
    destination: &crate::contracts::PullDestination,
    request: &crate::contracts::AdmissionRequest,
    ceiling: usize,
) -> Option<String> {
    use crate::contracts::LocalPullMutation as M;
    store
        .allocate_pull_request(destination, request, ceiling)
        .expect("allocate")?;
    let mut receipt = pull_receipt(request);
    if let Some(claim) = receipt.claim.as_mut() {
        claim.claim_id = format!("claim-{}", request.request_id);
        claim.task_id = format!("task-{}", request.request_id);
    }
    if let Some(task) = receipt.task.as_mut() {
        task.id = format!("task-{}", request.request_id);
    }
    store
        .mutate_local_pull(
            destination,
            &request.request_id,
            &M::Receive(Box::new(receipt)),
        )
        .expect("receive");
    Some(
        store
            .mutate_local_pull(destination, &request.request_id, &M::CreateLeaf)
            .expect("create leaf")
            .leaf_run_id
            .expect("leaf run"),
    )
}

/// [ORB-12617] Legacy and claimed admission share one ceiling.
///
/// The transitions the mixed drain has to get right are the two ends of a
/// wrapper's life: a wrapper whose lineage reaches a queued *claimed* leaf is
/// that leaf, not a second occupant, and the slot comes back when the claim
/// settles — not when the leaf goes terminal, because an unsettled terminal
/// leaf is still work the owner is holding a reservation for.
#[test]
fn mixed_wrapper_and_claimed_leaf_share_one_slot_until_the_claim_settles() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::mixed_wrapper_and_claimed_leaf_share_one_slot_until_the_claim_settles",
    ) {
        return;
    }
    use crate::contracts::{ClaimEvidence, ClaimMutation, LocalPullMutation as M};
    let (_temp, store, destination, request) = pull_fixture();

    let leaf = admit_queued_leaf(&store, &destination, &request, 4).expect("claimed leaf");
    let occupancy = store.drain_leaf_occupancy().expect("occupancy");
    assert_eq!(occupancy.occupied, 1);
    assert_eq!(occupancy.for_pipeline("task_claimed_local_pipeline"), 1);

    // wrapper -> gate -> that same queued claimed leaf. Three live runs, one
    // piece of work, one slot.
    let wrapper = store
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("wrapper");
    let gate = store
        .insert_job_run("task_gate_pipeline", 1, Utc::now(), None, None)
        .expect("gate");
    link_dispatch(&store, &wrapper, &gate.run_id, &gate.job_id);
    link_dispatch(&store, &gate, &leaf, "task_claimed_local_pipeline");
    let occupancy = store.drain_leaf_occupancy().expect("occupancy");
    assert_eq!(
        occupancy.occupied, 1,
        "a wrapper is replaced by the claimed leaf beneath it, not counted beside it"
    );
    assert_eq!(occupancy.for_pipeline("task_claimed_local_pipeline"), 1);

    // The leaf terminalizes. The claim is not settled, so the work still holds
    // its slot — and it is still one slot, not one for the record and one for
    // the wrapper that has lost its live descendant.
    store
        .mark_job_run_running(&leaf, Utc::now(), std::process::id())
        .expect("start leaf");
    store
        .finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
        .expect("terminal leaf");
    let occupancy = store.drain_leaf_occupancy().expect("occupancy");
    assert_eq!(
        occupancy.occupied, 1,
        "a terminal but unsettled claim keeps exactly the slot it already had"
    );
    assert_eq!(occupancy.for_pipeline("task_claimed_local_pipeline"), 1);

    // Settlement releases it. The wrapper is still live and now represents
    // itself again, which is one slot and not zero.
    let settlement = ClaimMutation::Fail(ClaimEvidence {
        summary: Some("fixture settlement".into()),
        ..Default::default()
    });
    store
        .mutate_local_pull(
            &destination,
            &request.request_id,
            &M::Settle(Box::new(settlement)),
        )
        .expect("settle");
    store
        .mutate_local_pull(&destination, &request.request_id, &M::Settled)
        .expect("settled");
    let occupancy = store.drain_leaf_occupancy().expect("occupancy");
    assert_eq!(occupancy.occupied, 1, "the live wrapper still holds a slot");
    assert_eq!(occupancy.for_pipeline("task_claimed_local_pipeline"), 0);
    store
        .mark_job_run_running(&wrapper.run_id, Utc::now(), std::process::id())
        .expect("start wrapper");
    store
        .finalize_job_run(&wrapper.run_id, JobRunState::Success, Utc::now(), None)
        .expect("finish wrapper");
    assert_eq!(store.drain_leaf_occupancy().expect("occupancy").occupied, 0);
}

/// [ORB-12617] The per-definition `max_active_runs: 10` is enforced against the
/// same reading, and it is *per definition*: ten claimed local leaves do not
/// consume the claimed PR definition's allowance, and a legacy leaf running
/// beside them is a separate definition again. What they do share is the
/// global ceiling.
#[test]
fn mixed_admission_respects_the_per_pipeline_maximum_and_the_shared_ceiling() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::mixed_admission_respects_the_per_pipeline_maximum_and_the_shared_ceiling",
    ) {
        return;
    }
    let (_temp, store, destination, request) = pull_fixture();
    // A legacy leaf is already running outside any wrapper.
    store
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .expect("legacy leaf");
    assert_eq!(
        store.drain_leaf_occupancy().expect("occupancy").occupied,
        1,
        "a legacy leaf occupies the same ceiling a claimed one does"
    );

    // Ten claimed local leaves fit under a generous global ceiling.
    for index in 0..10 {
        let mut next = request.clone();
        next.request_id = format!("local-{index}");
        assert!(
            admit_queued_leaf(&store, &destination, &next, 64).is_some(),
            "claimed local leaf {index} is within the definition's allowance"
        );
    }
    let occupancy = store.drain_leaf_occupancy().expect("occupancy");
    assert_eq!(occupancy.occupied, 11);
    assert_eq!(occupancy.for_pipeline("task_claimed_local_pipeline"), 10);
    assert_eq!(occupancy.for_pipeline("task_pr_pipeline"), 1);

    // The eleventh is refused by the definition's own maximum, with the
    // global ceiling nowhere near reached.
    let mut eleventh = request.clone();
    eleventh.request_id = "local-10".into();
    assert!(
        store
            .allocate_pull_request(&destination, &eleventh, 64)
            .expect("allocate")
            .is_none(),
        "the eleventh claimed local leaf exceeds that definition's max_active_runs"
    );

    // A different definition has its own allowance, and takes a shared slot.
    let mut pr = request.clone();
    pr.request_id = "pr-0".into();
    pr.ship.mode = "pr".into();
    assert!(
        admit_queued_leaf(&store, &destination, &pr, 64).is_some(),
        "the claimed PR definition has its own allowance"
    );
    assert_eq!(
        store.drain_leaf_occupancy().expect("occupancy").occupied,
        12
    );

    // ...but not its own ceiling: at the shared ceiling nothing is admitted,
    // whichever definition asks.
    let mut refused = request.clone();
    refused.request_id = "pr-1".into();
    refused.ship.mode = "pr".into();
    assert!(
        store
            .allocate_pull_request(&destination, &refused, 12)
            .expect("allocate")
            .is_none(),
        "the shared ceiling is shared: legacy and claimed leaves both count"
    );
}

/// [ORB-12617] An admission that has been requested but has no run yet is
/// capacity nothing else can see. It must hold a slot from the moment it is
/// durable, or a crash between request and leaf creation would let the next
/// pass over-admit.
#[test]
fn an_unrepresented_pending_admission_holds_a_slot_of_its_own() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::an_unrepresented_pending_admission_holds_a_slot_of_its_own",
    ) {
        return;
    }
    let (_temp, store, destination, request) = pull_fixture();
    store
        .allocate_pull_request(&destination, &request, 2)
        .expect("allocate")
        .expect("admitted");
    let occupancy = store.drain_leaf_occupancy().expect("occupancy");
    assert_eq!(occupancy.occupied, 1);
    assert_eq!(
        occupancy.for_pipeline("task_claimed_local_pipeline"),
        1,
        "a request with no run yet still counts against its definition"
    );
    let mut second = request.clone();
    second.request_id = "two".into();
    store
        .allocate_pull_request(&destination, &second, 2)
        .expect("allocate")
        .expect("second slot");
    assert_eq!(store.drain_leaf_occupancy().expect("occupancy").occupied, 2);
    let mut third = request.clone();
    third.request_id = "three".into();
    assert!(
        store
            .allocate_pull_request(&destination, &third, 2)
            .expect("allocate")
            .is_none(),
        "two unrepresented admissions fill a ceiling of two"
    );
}

/// [ORB-12617] A workspace that has never pulled must not grow pull schema
/// merely because the legacy drain asked how full it is.
#[test]
fn reading_occupancy_creates_no_pull_schema() {
    let store = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws");
    store
        .insert_job_run("task_local_pipeline", 1, Utc::now(), None, None)
        .expect("legacy leaf");
    let occupancy = store.drain_leaf_occupancy().expect("occupancy");
    assert_eq!(occupancy.occupied, 1);
    assert_eq!(occupancy.for_pipeline("task_local_pipeline"), 1);
    assert!(
        store
            .local_pull_for_run("missing")
            .expect("lookup")
            .is_none(),
        "the feature schema is still absent, so the run lookup short-circuits"
    );
}

#[test]
fn local_pull_idle_is_permanent_and_follower_local_is_refused() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::local_pull_idle_is_permanent_and_follower_local_is_refused",
    ) {
        return;
    }
    let (_temp, store, mut destination, request) = pull_fixture();
    destination.execution_machine_id = "follower".into();
    assert!(
        store
            .allocate_pull_request(&destination, &request, 10)
            .is_err()
    );
    destination.execution_machine_id = "owner".into();
    store
        .allocate_pull_request(&destination, &request, 1)
        .expect("allocate");
    let mut idle = pull_receipt(&request);
    idle.claim = None;
    idle.task = None;
    store
        .mutate_local_pull(
            &destination,
            "one",
            &crate::contracts::LocalPullMutation::Receive(Box::new(idle)),
        )
        .expect("idle");
    assert_eq!(
        store
            .allocate_pull_request(&destination, &request, 1)
            .expect("retry")
            .expect("receipt")
            .phase,
        crate::contracts::LocalPullPhase::Idle
    );
    let mut changed = request.clone();
    changed.ship.base_branch = "changed".into();
    assert!(
        store
            .allocate_pull_request(&destination, &changed, 1)
            .is_err()
    );
}

fn isolated_pull_test(name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_LOCAL_PULL_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(name) {
        return false;
    }
    let home = tempfile::tempdir().expect("isolated home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .expect("isolated pull child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{name}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "child did not execute exact test: {stdout}"
    );
    true
}

#[test]
fn local_pull_concurrent_allocation_obeys_shared_ceiling() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::local_pull_concurrent_allocation_obeys_shared_ceiling",
    ) {
        return;
    }
    let (_temp, store, destination, request) = pull_fixture();
    // Bootstrap the feature before independent connections race for admission.
    store.local_pull_admissions().expect("initialize");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
    let mut workers = Vec::new();
    for index in 0..12 {
        let jobs = store.clone();
        let destination = destination.clone();
        let mut request = request.clone();
        request.request_id = format!("request-{index}");
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            jobs.allocate_pull_request(&destination, &request, 3)
                .expect("allocate")
                .is_some()
        }));
    }
    let admitted = workers
        .into_iter()
        .map(|worker| usize::from(worker.join().expect("join")))
        .sum::<usize>();
    assert_eq!(admitted, 3);
    assert_eq!(store.local_pull_admissions().expect("pending").len(), 3);
}

#[test]
fn local_pull_pipeline_limit_and_live_parent_reduction_are_authoritative() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::backend::local_pull_pipeline_limit_and_live_parent_reduction_are_authoritative",
    ) {
        return;
    }
    let (_temp, store, destination, request) = pull_fixture();
    for index in 0..11 {
        let mut next = request.clone();
        next.request_id = format!("request-{index}");
        assert_eq!(
            store
                .allocate_pull_request(&destination, &next, 20)
                .expect("allocate")
                .is_some(),
            index < 10
        );
    }
    let mut pr = request.clone();
    pr.ship.mode = "pr".into();
    pr.request_id = "pr".into();
    assert!(
        store
            .allocate_pull_request(&destination, &pr, 20)
            .expect("other pipeline")
            .is_some()
    );
    store
        .update_run_state(&request.run_context.run_id, &mut |_, state| {
            assert!(state.set_drain_worker_limit(1, 20, "operator".into(), None, None));
            Ok(())
        })
        .expect("reduce live limit");
    pr.request_id = "after-reduction".into();
    assert!(
        store
            .allocate_pull_request(&destination, &pr, 20)
            .expect("live ceiling")
            .is_none()
    );
}
