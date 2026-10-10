//! `invoke_and_wait` hands its parent a bounded child summary, so a finished
//! fan-out parent stores no copy of any child's pipeline [ORB-14587].

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_engine::RuntimeHost;
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

use super::super::invoke::invoke_and_wait_with;
use super::super::pipeline_success_guard;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_config;

/// A finished four-pilot `ci_failure_sweep_pipeline` run in the encoding
/// before compaction: each pilot's wait entry carries the child's whole
/// pipeline and is held three times (`pilots`, its `pilot_results` alias and
/// `step_outputs`), and the CI evidence twice. Recorded from a live sweep with
/// every content string replaced by filler; structure and duplication are the
/// recorded run's.
const RECORDED_SWEEP: &str = include_str!("fixtures/ci_sweep_four_pilots_state.json");

#[test]
fn a_finished_ci_sweep_stores_child_summaries_and_no_resume_state() {
    let (_root, runtime, _) = runtime_with_workspace_config(None);
    let jobs = runtime.stores().jobs();
    let parent = jobs
        .insert_job_run("ci_failure_sweep_pipeline", 1, Utc::now(), None, None)
        .expect("insert sweep");
    jobs.mark_job_run_running(&parent.run_id, Utc::now(), std::process::id())
        .expect("start sweep");
    let mut recorded: PipelineState =
        serde_json::from_str(RECORDED_SWEEP).expect("recorded sweep state");
    recorded.run_id = parent.run_id.clone();
    let recorded_bytes = serde_json::to_string(&recorded)
        .expect("encode recorded state")
        .len();
    runtime
        .write_run_state(&parent.run_id, &recorded)
        .expect("seed sweep state");
    let entries = recorded.pipeline["pilots"]
        .as_array()
        .expect("recorded pilot entries")
        .clone();
    assert_eq!(entries.len(), 4);

    // Each pilot, as `invoke_and_wait` now returns its child's wait entry.
    let summaries = entries
        .iter()
        .map(|entry| {
            assert!(entry["pipeline"].is_object(), "recorded entry: {entry}");
            invoke_and_wait_with(
                &runtime,
                "invoke_and_wait",
                &json!({
                    "job_name": "task_pilot_pipeline",
                    "run_input": {},
                    "run_id": parent.run_id,
                    "step_id": "pilot",
                }),
                |_| Ok(json!({"run_id": entry["run_id"], "queued": false})),
                |_| Ok(json!({"results": [entry]})),
            )
            .expect("pilot wait")
        })
        .collect::<Vec<_>>();
    for (summary, entry) in summaries.iter().zip(&entries) {
        assert_eq!(
            summary,
            &json!({
                "run_id": entry["run_id"],
                "status": entry["status"],
                "finished_at": entry["finished_at"],
            }),
            "the summary keeps what guards and readers use, never the child pipeline"
        );
    }
    let guard = pipeline_success_guard(
        "pipeline_success_guard",
        &json!({"context": "ci-failure sweep pilot child", "results": summaries}),
    )
    .expect("the sweep's guard passes on summaries");
    assert_eq!(guard["checked_count"], 4);

    // The fan-in checkpoint and the terminal write, as the engine makes them.
    let collected = Value::Array(summaries);
    runtime
        .checkpoint_step(
            &parent.run_id,
            2,
            "pilots",
            &collected,
            &BTreeMap::from([("pilot_results".to_string(), collected.clone())]),
        )
        .expect("fan-in checkpoint");
    jobs.finalize_job_run(&parent.run_id, JobRunState::Success, Utc::now(), Some(0))
        .expect("finish sweep");

    let stored_bytes = runtime
        .sqlite_store()
        .expect("store")
        .connection()
        .lock()
        .expect("store connection")
        .query_row(
            "SELECT length(pipeline_state_json) FROM job_run_states WHERE run_id = ?1",
            [&parent.run_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("stored state length");
    let stored_bytes = usize::try_from(stored_bytes).expect("length");
    assert!(
        stored_bytes * 10 <= recorded_bytes * 4,
        "a finished four-pilot sweep must store at least 60% less state than the recorded \
         encoding: {stored_bytes} of {recorded_bytes} bytes"
    );

    let state = runtime
        .read_run_state(&parent.run_id)
        .expect("read sweep")
        .expect("sweep state");
    assert!(state.step_outputs.is_empty() && state.compound_outputs.is_empty());
    assert_eq!(state.step_output(2), Some(&collected));
    assert_eq!(state.pipeline["pilot_results"], collected);
    assert_eq!(
        state.step_output(0),
        Some(&recorded.pipeline["collect"]),
        "outputs still held in the pipeline stay readable by step"
    );
    let lineage = |state: &PipelineState| {
        state
            .child_dispatches
            .iter()
            .map(|dispatch| (dispatch.child_run_id.clone(), dispatch.child_status.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lineage(&state),
        lineage(&recorded),
        "child lineage survives"
    );
}
