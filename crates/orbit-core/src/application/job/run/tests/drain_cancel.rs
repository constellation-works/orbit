//! Deterministic stop-time interleavings: work persisted by the selected
//! drain before its stop is confirmed must not escape forced cancellation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::{
    AdmissionReceipt, AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    AdmissionTaskSummary, ClaimMutation, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, ExecutionClaim,
    ExecutionClaimPhase, ExecutionLocation, LocalPullAdmission, LocalPullMutation, LocalPullPhase,
    PullDestination,
};
use orbit_store::contracts::{TaskCreateParams, TaskReservationReleaseReason};
use orbit_tools::{DrainOwnerTransport, OwnerCoordinator};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{ChildDispatch, JobRun, JobRunState, PipelineState};
use serde_json::{Value, json};

use super::super::actions::CancellationRequest;
use super::{insert_pending_run, test_runtime};
use crate::OrbitRuntime;
use crate::application::tests::run_isolated_test;

/// State at the owner transport boundary: a received Release removes the
/// claim. Retain receipts to exercise an admission whose reply was lost.
#[derive(Default)]
struct Owner {
    receipts: Mutex<BTreeMap<String, AdmissionReceipt>>,
    claims: Mutex<BTreeSet<String>>,
}

impl DrainOwnerTransport for Owner {
    fn call(&self, _selector: &str, name: &str, input: Value) -> Result<Value, OrbitError> {
        match name {
            "orbit.drain.receipt.lookup" => {
                let receipts = self.receipts.lock().unwrap();
                let receipt = receipts.get(input["request_id"].as_str().unwrap()).unwrap();
                Ok(json!({"outcome": "found", "receipt": receipt}))
            }
            "orbit.drain.claim.settle" => {
                let settlement: ClaimMutation =
                    serde_json::from_value(input["settlement"].clone()).unwrap();
                assert!(matches!(settlement, ClaimMutation::Release(_)), "{input}");
                assert!(
                    self.claims
                        .lock()
                        .unwrap()
                        .remove(input["claim_id"].as_str().unwrap()),
                    "only a live claim can be released: {input}"
                );
                Ok(json!({"phase": "revoked"}))
            }
            _ => panic!("unexpected owner call: {name}"),
        }
    }

    fn show_task(&self, _: &str, _: Value) -> Result<Value, OrbitError> {
        panic!("cancellation does not read an owner task")
    }

    fn worker_coordinator(&self) -> Arc<dyn OwnerCoordinator> {
        Arc::new(Self::default())
    }
}

impl OwnerCoordinator for Owner {
    fn call(&self, _: &str, _: Value, _: ToolSessionContext) -> Result<Value, OrbitError> {
        panic!("no worker starts in the stop-time interleaving")
    }
}

fn destination() -> PullDestination {
    PullDestination {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "workspace".into(),
        selector: "owner/workspace".into(),
        execution_machine_id: "follower".into(),
    }
}

fn drain(runtime: &OrbitRuntime, job: &str, input: Value) -> JobRun {
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(job, 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input),
        )
        .unwrap();
    // Only the injected parent stop sees this PID; it never signals it.
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    run
}

fn in_progress_task(runtime: &OrbitRuntime, run: &JobRun) -> String {
    let task = runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".into(),
            parent_id: None,
            title: "cancel candidate".into(),
            description: "candidate should remain available".into(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: "Resume the candidate after cancellation".into(),
            execution_summary: String::new(),
            context_files: vec!["file:src/candidate.rs".into()],
            repo_root: None,
            created_by: Some("test".into()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: Some(TaskComplexity::Low),
            task_type: TaskType::Bug,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
            context_creation: Vec::new(),
        })
        .expect("create task");
    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(run.run_id.clone()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("couple task to run");
    task.id
}

#[test]
fn operator_leaf_cancel_requeues_with_reason_by_default_and_block_preserves_legacy_behavior() {
    if run_isolated_test(std::any::type_name_of_val(
        &operator_leaf_cancel_requeues_with_reason_by_default_and_block_preserves_legacy_behavior,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();

    let run = insert_pending_run(&runtime, "task_pr_pipeline");
    let task_id = in_progress_task(&runtime, &run);
    let cancelled = runtime
        .cancel_job_run_with_options_and_signal(
            &run.run_id,
            CancellationRequest {
                actor: "operator",
                source: "fixture",
                reason: Some("preserve the candidate for later"),
                block_task: false,
            },
            false,
            |_| unreachable!("pending run must not signal its owner"),
        )
        .expect("cancel task leaf");
    assert_eq!(cancelled.outcome, "cancelled");
    let task = runtime.get_task(&task_id).expect("task after cancel");
    assert_eq!(task.status, TaskStatus::Backlog);
    assert_eq!(task.plan, "Resume the candidate after cancellation");
    assert_eq!(
        task.context_files,
        vec!["file:src/candidate.rs".to_string()]
    );
    assert_eq!(task.job_run_id.as_deref(), Some(run.run_id.as_str()));
    let cancellation_policy = runtime
        .read_run_state(&run.run_id)
        .expect("run state")
        .and_then(|state| state.task_cancellation_policy)
        .expect("durable cancellation policy");
    assert!(!cancellation_policy.block);
    assert!(
        cancellation_policy
            .note
            .contains("preserve the candidate for later")
    );
    let history = runtime.get_task_history(&task_id).expect("task history");
    assert_eq!(
        history.last().expect("cancel history").event,
        "workflow_run_cancelled"
    );
    assert!(
        history
            .last()
            .expect("cancel history")
            .note
            .as_deref()
            .is_some_and(|note| note.contains("preserve the candidate for later"))
    );

    let run = insert_pending_run(&runtime, "task_pr_pipeline");
    let task_id = in_progress_task(&runtime, &run);
    runtime
        .cancel_job_run_with_options_and_signal(
            &run.run_id,
            CancellationRequest {
                actor: "operator",
                source: "fixture",
                reason: Some("keep the old blocked behavior"),
                block_task: true,
            },
            false,
            |_| unreachable!("pending run must not signal its owner"),
        )
        .expect("cancel task leaf with block");
    let task = runtime.get_task(&task_id).expect("blocked task");
    assert_eq!(task.status, TaskStatus::Blocked);
    assert_eq!(
        task.context_files,
        vec!["file:src/candidate.rs".to_string()]
    );
    assert!(
        runtime
            .get_task_history(&task_id)
            .expect("task history")
            .last()
            .is_some_and(|entry| entry
                .note
                .as_deref()
                .is_some_and(|note| { note.contains("keep the old blocked behavior") }))
    );
}

#[test]
fn cancellation_policy_is_durable_before_the_owner_is_signalled() {
    if run_isolated_test(std::any::type_name_of_val(
        &cancellation_policy_is_durable_before_the_owner_is_signalled,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "task_pr_pipeline");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark run running");
    let task_id = in_progress_task(&runtime, &run);

    let cancelled = runtime
        .cancel_job_run_with_options_and_signal(
            &run.run_id,
            CancellationRequest {
                actor: "operator",
                source: "fixture",
                reason: Some("worker saw the cancel request"),
                block_task: false,
            },
            false,
            |_| {
                runtime
                    .finalize_job_run_with_reservation_cleanup(
                        &run.run_id,
                        JobRunState::Cancelled,
                        Utc::now(),
                        Some(1),
                        TaskReservationReleaseReason::RunTerminal,
                    )
                    .expect("worker terminalizes during cancellation signal");
                Ok("worker_observed_cancel".into())
            },
        )
        .expect("cancel run");

    assert_eq!(cancelled.outcome, "already_terminal");
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Backlog
    );
    assert!(
        runtime
            .get_task_history(&task_id)
            .expect("task history")
            .last()
            .is_some_and(|entry| entry
                .note
                .as_deref()
                .is_some_and(|note| { note.contains("worker saw the cancel request") }))
    );
}

fn admission(
    runtime: &OrbitRuntime,
    owner: &Owner,
    drain: &JobRun,
    phase: LocalPullPhase,
) -> LocalPullAdmission {
    let destination = destination();
    let jobs = runtime.stores().jobs();
    let request = AdmissionRequest {
        request_id: format!("request-{}", owner.receipts.lock().unwrap().len()),
        caller_version: "fixture".into(),
        caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_fingerprint: None,
        caller_before_pr: false,
        review_gate: false,
        run_context: AdmissionRunContext {
            run_id: drain.run_id.clone(),
            job_name: drain.job_id.clone(),
            machine_name: None,
        },
        ship: AdmissionShipContract {
            mode: "pr".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            before_pr: false,
            completion: "review".into(),
            authorization_reference: None,
            review: None,
        },
        crews: None,
        os: None,
    };
    let record = jobs
        .allocate_pull_request(&destination, &request, 10)
        .unwrap()
        .unwrap();
    let id = record.request.request_id.clone();
    let claim_id = format!("claim-{id}");
    let receipt = AdmissionReceipt {
        schema_version: 1,
        request: record.request.clone(),
        machine_id: destination.execution_machine_id.clone(),
        claim: Some(ExecutionClaim {
            claim_id: claim_id.clone(),
            task_id: format!("task-{id}"),
            request_id: id.clone(),
            executed_on: ExecutionLocation {
                machine_id: destination.execution_machine_id.clone(),
                machine_name: None,
            },
            run_context: record.request.run_context.clone(),
            footprint: vec![],
            reservation_id: format!("reservation-{id}"),
            reservation_expires_at: (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            phase: ExecutionClaimPhase::Claimed,
        }),
        task: Some(AdmissionTaskSummary {
            id: format!("task-{id}"),
            title: "stop-time admission".into(),
            complexity: None,
            crew: None,
            context_files: vec![],
        }),
        invalid_candidates: vec![],
        deferred_conflicts: vec![],
        crew_unavailable: vec![],
        os_unavailable: vec![],
        queue_depth: 0,
    };
    owner.claims.lock().unwrap().insert(claim_id);
    owner
        .receipts
        .lock()
        .unwrap()
        .insert(id.clone(), receipt.clone());
    if phase == LocalPullPhase::Requested {
        return record;
    }
    let mut record = jobs
        .mutate_local_pull(
            &destination,
            &id,
            &LocalPullMutation::Receive(Box::new(receipt)),
        )
        .unwrap();
    if phase == LocalPullPhase::Claimed {
        return record;
    }
    // Launch intent is durable while the leaf is still pending, immediately
    // before the worker is started. No real process is needed for this race.
    for mutation in [
        LocalPullMutation::CreateLeaf,
        LocalPullMutation::Bound,
        LocalPullMutation::LaunchIntent,
    ] {
        record = jobs
            .mutate_local_pull(&destination, &id, &mutation)
            .unwrap();
    }
    record
}

#[test]
fn forced_pull_cancel_releases_admissions_persisted_during_stop_and_keeps_inherited_work() {
    if run_isolated_test(std::any::type_name_of_val(
        &forced_pull_cancel_releases_admissions_persisted_during_stop_and_keeps_inherited_work,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let owner = Arc::new(Owner::default());
    let runtime = runtime.with_drain_owner_transport(owner.clone());
    let previous = drain(
        &runtime,
        "workspace_pull_pipeline",
        json!({"destination": destination()}),
    );
    let inherited = admission(&runtime, &owner, &previous, LocalPullPhase::Claimed);
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&previous.run_id, JobRunState::Cancelled, Utc::now(), None)
        .unwrap();
    let selected = drain(
        &runtime,
        "workspace_pull_pipeline",
        json!({"destination": destination()}),
    );
    assert_eq!(
        runtime.pull_drain_admissions(&selected.run_id).unwrap(),
        vec![inherited]
    );

    let cancel = runtime
        .cancel_job_run_with_options_and_signal(
            &selected.run_id,
            CancellationRequest {
                actor: "operator",
                source: "fixture",
                reason: None,
                block_task: true,
            },
            true,
            |_| {
                // This runs after the initial carried snapshot, while the worker
                // is stopping. The owner committed even the unanswered request.
                for phase in [LocalPullPhase::Requested, LocalPullPhase::Claimed] {
                    admission(&runtime, &owner, &selected, phase);
                }
                Ok("confirmed_fixture_stop".into())
            },
        )
        .unwrap();

    assert_eq!(cancel.outcome, "cancelled");
    assert_eq!(
        cancel.pull_settlements.len(),
        3,
        "late and inherited claims must all settle"
    );
    assert!(
        owner.claims.lock().unwrap().is_empty(),
        "no owner claim may be stranded after confirmed stop"
    );
    let records = runtime.stores().jobs().local_pull_admissions().unwrap();
    assert_eq!(records.len(), 3);
    assert!(
        records
            .iter()
            .all(|record| record.phase == LocalPullPhase::Settled
                && matches!(record.settlement, Some(ClaimMutation::Release(_))))
    );
    assert!(
        runtime
            .stores()
            .jobs()
            .unsettled_local_pull_admissions()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn forced_pull_cancel_stops_a_late_launch_without_touching_another_live_drains_admission() {
    if run_isolated_test(std::any::type_name_of_val(
        &forced_pull_cancel_stops_a_late_launch_without_touching_another_live_drains_admission,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let owner = Arc::new(Owner::default());
    let runtime = runtime.with_drain_owner_transport(owner.clone());
    let selected = drain(
        &runtime,
        "workspace_pull_pipeline",
        json!({"destination": destination()}),
    );
    let other = drain(
        &runtime,
        "workspace_pull_pipeline",
        json!({"destination": destination()}),
    );
    let theirs = admission(&runtime, &owner, &other, LocalPullPhase::Launching);
    assert!(
        runtime
            .pull_drain_admissions(&selected.run_id)
            .unwrap()
            .is_empty()
    );
    let mut late_leaf = None;
    let cancel = runtime
        .cancel_job_run_with_options_and_signal(
            &selected.run_id,
            CancellationRequest {
                actor: "operator",
                source: "fixture",
                reason: None,
                block_task: true,
            },
            true,
            |_| {
                late_leaf =
                    admission(&runtime, &owner, &selected, LocalPullPhase::Launching).leaf_run_id;
                Ok("confirmed_fixture_stop".into())
            },
        )
        .unwrap();

    assert_eq!(cancel.forced_runs, vec![late_leaf.clone().unwrap()]);
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .get_job_run(&late_leaf.unwrap())
            .unwrap()
            .unwrap()
            .state,
        JobRunState::Cancelled
    );
    let remaining = runtime
        .stores()
        .jobs()
        .unsettled_local_pull_admissions()
        .unwrap();
    assert_eq!(
        remaining,
        vec![theirs.clone()],
        "another live drain for the same owner must keep its admission"
    );
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .get_job_run(theirs.leaf_run_id.as_ref().unwrap())
            .unwrap()
            .unwrap()
            .state,
        JobRunState::Pending
    );
    assert_eq!(
        *owner.claims.lock().unwrap(),
        BTreeSet::from([theirs.receipt.unwrap().claim.unwrap().claim_id])
    );
}

fn record_child(runtime: &OrbitRuntime, parent: &JobRun, child: &str) {
    let mut state = runtime.read_run_state(&parent.run_id).unwrap().unwrap();
    state.record_child_dispatch(ChildDispatch::submitted(
        child.into(),
        "task_auto_pipeline".into(),
        "invoke_detached".into(),
        false,
        false,
        Utc::now(),
    ));
    runtime.write_run_state(&parent.run_id, &state).unwrap();
}

#[test]
fn forced_local_cancel_handles_children_persisted_during_stop_and_preserves_other_drains() {
    if run_isolated_test(std::any::type_name_of_val(
        &forced_local_cancel_handles_children_persisted_during_stop_and_preserves_other_drains,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let selected = drain(&runtime, "workspace_auto_pipeline", json!({}));
    let other = drain(&runtime, "workspace_auto_pipeline", json!({}));
    let theirs = insert_pending_run(&runtime, "task_auto_pipeline");
    record_child(&runtime, &other, &theirs.run_id);
    let stoppable = insert_pending_run(&runtime, "task_auto_pipeline");
    let unstoppable = drain(&runtime, "task_auto_pipeline", json!({}));
    let missing = "missing-child";
    assert_eq!(
        runtime
            .read_run_state(&selected.run_id)
            .unwrap()
            .unwrap()
            .open_child_dispatches()
            .count(),
        0
    );

    let cancel = runtime
        .cancel_job_run_with_options_and_signal(
            &selected.run_id,
            CancellationRequest {
                actor: "operator",
                source: "fixture",
                reason: None,
                block_task: true,
            },
            true,
            |_| {
                for child in [&stoppable.run_id, &unstoppable.run_id, missing] {
                    record_child(&runtime, &selected, child);
                }
                Ok("confirmed_fixture_stop".into())
            },
        )
        .unwrap();

    assert_eq!(cancel.forced_runs, vec![stoppable.run_id.clone()]);
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .get_job_run(&stoppable.run_id)
            .unwrap()
            .unwrap()
            .state,
        JobRunState::Cancelled
    );
    assert_eq!(
        cancel
            .unstopped_children
            .iter()
            .map(|child| child.child_run_id.as_str())
            .collect::<Vec<_>>(),
        vec![unstoppable.run_id.as_str(), missing]
    );
    assert!(
        cancel
            .unstopped_children
            .iter()
            .all(|child| !child.reason.is_empty())
    );
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .get_job_run(&theirs.run_id)
            .unwrap()
            .unwrap()
            .state,
        JobRunState::Pending
    );
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .get_job_run(&other.run_id)
            .unwrap()
            .unwrap()
            .state,
        JobRunState::Running
    );
    assert_eq!(
        runtime
            .read_run_state(&selected.run_id)
            .unwrap()
            .unwrap()
            .open_child_dispatches()
            .count(),
        0,
        "the final scan must include lineage closed by the parent's cancellation"
    );
}
