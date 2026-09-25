//! Sibling tests for run-state checkpoints and rebase recovery certification.

use chrono::Utc;
use orbit_engine::RuntimeHost;
use serde_json::json;

use super::task_automation::test_runtime;
use crate::OrbitRuntime;

#[test]
fn recovered_rebase_checkpoint_survives_restart_without_completing_the_step() {
    use orbit_types::workflow::{JobRunState, PipelineState};

    let (root, runtime) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    let mut state = PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        json!({"task_ids": ["T-recovery"]}),
    );
    state.record_step(
        3,
        JobRunState::Success,
        Some(json!({"head_sha": "before"})),
        None,
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &state)
        .unwrap();
    let output = json!({
        "run_id": run.run_id,
        "step_id": "sync_base",
        "task_ids": ["T-recovery"],
        "workspace_path": root.path().join("repo"),
        "head_sha_before": "before",
        "head_sha": "after",
        "base_sha": "target",
        "remote_sha_before": "remote",
    });
    runtime
        .checkpoint_rebase_recovery(&run.run_id, "sync_base", &output)
        .unwrap();
    drop(runtime);

    let reopened = OrbitRuntime::from_roots(
        &root.path().join("global"),
        &root.path().join("repo/.orbit"),
    )
    .unwrap();
    let durable = RuntimeHost::read_run_state(&reopened, &run.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(durable.rebase_recovery_checkpoints["sync_base"], output);
    assert_eq!(durable.step_outputs, state.step_outputs);
    assert_eq!(durable.step_states, state.step_states);
    assert_eq!(durable.next_step_index, 4);
    assert!(durable.failure_activity_checkpoint.is_none());
    assert!(
        reopened
            .checkpoint_rebase_recovery("missing-run", "sync_base", &output)
            .is_err()
    );

    // The certificate is durable too, and it is what authenticates the run-row
    // copy after the restart.
    assert!(
        RuntimeHost::verify_rebase_recovery(&reopened, &run.run_id, "sync_base", &output).unwrap(),
    );

    // Rewrite the run row the way a leaf holding the `orbit.db` grant would.
    // The store accepts the bytes; the authority does not accept the claim.
    let mut forged = durable.clone();
    forged
        .rebase_recovery_checkpoints
        .get_mut("sync_base")
        .unwrap()["head_sha"] = json!("leaf-authored");
    reopened
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &forged)
        .unwrap();
    let stored = RuntimeHost::read_run_state(&reopened, &run.run_id)
        .unwrap()
        .unwrap();
    let stored = &stored.rebase_recovery_checkpoints["sync_base"];
    assert_eq!(stored["head_sha"], json!("leaf-authored"));
    assert!(
        !RuntimeHost::verify_rebase_recovery(&reopened, &run.run_id, "sync_base", stored).unwrap(),
        "a leaf-authored run-store row carries no certificate",
    );
}
