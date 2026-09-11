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
