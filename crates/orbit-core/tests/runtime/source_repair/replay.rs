//! Lost-reply reconciliation through the public operator request.
use super::fixture::Fixture;
use chrono::Utc;
use orbit_engine::RuntimeHost;
use orbit_types::workflow::JobRunState;
use serde_json::json;

pub(super) fn lost_spawn_reply_reconciles_the_reserved_run_after_older_retries() {
    let f = Fixture::new();
    // Historical retries are real store rows, with no workers or source effects.
    for _ in 0..5 {
        let mut run = f
            .runtime
            .insert_job_run(
                "task_pr_pipeline",
                2,
                Utc::now(),
                Some(json!({"task_ids":[f.task]})),
                Some(f.source.run_id.clone()),
            )
            .unwrap();
        run.state = JobRunState::Failed;
        run.finished_at = Some(Utc::now());
        f.runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &run, None)
            .unwrap();
    }
    let request = f.submit(f.inspect());
    let missing = f._root.path().join("missing-worker");
    orbit_core::test_support::install_substitute_pipeline_worker([missing
        .to_string_lossy()
        .into_owned()]);
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request.clone())
        .unwrap_err();
    assert!(
        error.to_string().contains("spawn pipeline worker"),
        "{error}"
    );
    let history = f.runtime.get_task_history(&f.task).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|e| e.event == "source_repair_reserved")
            .count(),
        1
    );
    assert!(!history.iter().any(|e| e.event == "source_repair_linked"));
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request.clone())
        .unwrap();
    assert_eq!(replay["replayed"], true);
    let state: JobRunState = serde_json::from_value(replay["state"].clone()).unwrap();
    assert!(
        state.is_terminal(),
        "a replay reconciles the stopped attempt without restarting it"
    );
    let again = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request)
        .unwrap();
    assert_eq!(again["run_id"], replay["run_id"]);
    assert_eq!(
        f.runtime.job_history("task_pr_pipeline").unwrap().len(),
        7,
        "reconciliation must not create another run or spawn the restored worker"
    );
    f.assert_original();
}
