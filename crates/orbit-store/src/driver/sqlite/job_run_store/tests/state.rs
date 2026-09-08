use chrono::Utc;
use orbit_types::workflow::{JobRunState, PipelineState};

use super::super::SqliteJobRunStore;
use crate::Store;
use crate::contracts::JobRunStoreBackend;

/// [ORB-10002] Checkpoint storage round-trip: per-step recovery metadata
/// written into `pipeline_state_json` survives reload, and finalizing to
/// `interrupted` is a valid transition out of `running`.
#[test]
fn pipeline_state_checkpoints_round_trip_and_interrupted_finalize() {
    let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
    let scheduled_at = Utc::now();
    let run = backend
        .insert_job_run("job-ckpt", 1, scheduled_at, None, None)
        .expect("insert");
    assert!(
        backend
            .mark_job_run_running(&run.run_id, scheduled_at, 42)
            .expect("running")
            .owns_execution()
    );

    let mut state = PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        serde_json::json!({"seconds": 0}),
    );
    state.record_step(
        0,
        JobRunState::Success,
        Some(serde_json::json!({"ok": true})),
        None,
    );
    state.sync_pipeline(serde_json::json!({"s0": {"ok": true}}));
    backend
        .write_run_state(&run.run_id, &state)
        .expect("write checkpoint state");

    let loaded = backend
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    assert_eq!(loaded.step_states.get(&0), Some(&JobRunState::Success));
    assert_eq!(
        loaded.step_outputs.get(&0),
        Some(&serde_json::json!({"ok": true}))
    );
    assert_eq!(loaded.next_step_index, 1);
    assert_eq!(loaded.pipeline, serde_json::json!({"s0": {"ok": true}}));

    assert!(
        backend
            .finalize_job_run(&run.run_id, JobRunState::Interrupted, Utc::now(), Some(1))
            .expect("finalize interrupted")
    );
    let interrupted = backend
        .get_job_run(&run.run_id)
        .expect("get")
        .expect("some");
    assert_eq!(interrupted.state, JobRunState::Interrupted);
    // Checkpoint state survives finalization for a later resume.
    assert!(
        backend
            .read_run_state(&run.run_id)
            .expect("read state after finalize")
            .is_some()
    );
}
