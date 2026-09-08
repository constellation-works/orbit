use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use orbit_types::workflow::{JobRunState, JobTargetType, KnowledgeRunMetrics};
use tempfile::TempDir;

use super::super::SqliteJobRunStore;
use crate::Store;
use crate::contracts::{JobRunStepParams, JobRunStoreBackend};

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
