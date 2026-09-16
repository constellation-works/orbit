use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::{JobRunState, PipelineState, RunStateUpdate};

use super::super::SqliteJobRunStore;
use crate::Store;
use crate::contracts::{
    ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams, JobRunStoreBackend,
};

fn raw_pipeline_state_json(store: &Store, workspace_id: &str, run_id: &str) -> String {
    store
        .with_transaction(|tx| {
            tx.connection()
                .query_row(
                    "SELECT pipeline_state_json FROM job_runs \
                     WHERE workspace_id = ?1 AND run_id = ?2",
                    rusqlite::params![workspace_id, run_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(|e| OrbitError::Store(e.to_string()))
        })
        .expect("read raw pipeline_state_json")
        .expect("pipeline_state_json present")
}

fn assert_compact_pipeline_state_json(raw: &str, state: &PipelineState) {
    assert!(
        !raw.contains('\n'),
        "pipeline_state_json must be compact, got {raw}"
    );
    let expected = serde_json::to_string(state).expect("serialize compact");
    assert_eq!(raw, expected);
}

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

/// Pretty-printed rows remain readable; checkpoint and RMW writes store compact JSON.
#[test]
fn pipeline_state_json_writes_compact_and_pretty_rows_still_read() {
    let store = Store::open_in_memory().expect("store");
    let backend = SqliteJobRunStore::new(store.clone(), "ws_a");
    let scheduled_at = Utc::now();
    let run = backend
        .insert_job_run("job-compact", 1, scheduled_at, None, None)
        .expect("insert");
    assert!(
        backend
            .mark_job_run_running(&run.run_id, scheduled_at, 7)
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
    let pretty = serde_json::to_string_pretty(&state).expect("pretty seed");
    assert!(
        pretty.contains('\n'),
        "pretty seed must contain whitespace newlines"
    );
    store
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET pipeline_state_json = ?3 \
                     WHERE workspace_id = ?1 AND run_id = ?2",
                    rusqlite::params!["ws_a", &run.run_id, pretty],
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            Ok(())
        })
        .expect("seed pretty pipeline_state_json");

    let loaded = backend
        .read_run_state(&run.run_id)
        .expect("read pretty row")
        .expect("state exists");
    assert_eq!(loaded, state);

    backend
        .write_run_state(&run.run_id, &state)
        .expect("compact write");
    assert_compact_pipeline_state_json(
        &raw_pipeline_state_json(&store, "ws_a", &run.run_id),
        &state,
    );

    let updated = backend
        .update_run_state(&run.run_id, &mut |_, pipeline| {
            pipeline.record_step(
                1,
                JobRunState::Success,
                Some(serde_json::json!({"ok": true})),
                None,
            );
            Ok(())
        })
        .expect("rmw write");
    assert_eq!(updated, RunStateUpdate::Updated);
    let after_rmw = backend
        .read_run_state(&run.run_id)
        .expect("read after rmw")
        .expect("state exists");
    assert_compact_pipeline_state_json(
        &raw_pipeline_state_json(&store, "ws_a", &run.run_id),
        &after_rmw,
    );

    match backend
        .admit_child_job_run(&ChildJobRunAdmissionParams {
            parent_run_id: run.run_id.clone(),
            parent_step_id: Some("leaf".to_string()),
            job_id: "job-child".to_string(),
            action: "invoke_detached".to_string(),
            blocking: false,
            attempt: 1,
            scheduled_at,
            input: None,
            authority: None,
        })
        .expect("admit child")
    {
        ChildJobRunAdmissionOutcome::Admitted(_) => {}
        other => panic!("expected admitted child, got {other:?}"),
    }
    let after_admit = backend
        .read_run_state(&run.run_id)
        .expect("read after admit")
        .expect("state exists");
    assert_compact_pipeline_state_json(
        &raw_pipeline_state_json(&store, "ws_a", &run.run_id),
        &after_admit,
    );
}
