//! A drain does not re-admit a task whose own leaf failed at job load.

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_store::contracts::{JobRunStepParams, JobRunStoreBackend};
use orbit_types::workflow::{ChildDispatch, JobRunState, JobTargetType, PipelineState};
use serde_json::json;

use super::{Seed, classify, isolated, last_pass, running_run, runtime, seed};

const LOAD_ERROR: &str = "invalid input: script manifest .orbit/resources/scripts/fixture.yaml: entry 'scripts/missing.mjs' does not resolve to a regular file inside the workspace";

fn jobs(runtime: &OrbitRuntime) -> std::sync::Arc<dyn JobRunStoreBackend> {
    orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    )
}

/// A finished `task_auto_pipeline` leaf of `drain` carrying `task_id`.
///
/// `at_load` records only the job diagnostic step a load failure leaves;
/// otherwise the leaf records a real activity step, as a leaf that started
/// work and then failed does.
fn failed_leaf(runtime: &OrbitRuntime, drain: &str, task_id: &str, at_load: bool) -> String {
    let jobs = jobs(runtime);
    let run = jobs
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": [task_id] })),
            None,
        )
        .unwrap();
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({})),
        )
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    let now = Utc::now();
    let (target_type, target_id, error) = if at_load {
        (
            JobTargetType::Job,
            "task_auto_pipeline".to_string(),
            Some(LOAD_ERROR.to_string()),
        )
    } else {
        (
            JobTargetType::Activity,
            "list_backlog_tasks".to_string(),
            Some("transient store error".to_string()),
        )
    };
    jobs.complete_job_run_step(
        &run.run_id,
        &JobRunStepParams {
            lock_wait_ms: None,
            step_index: 0,
            target_type,
            target_id,
            started_at: now - Duration::milliseconds(15),
            finished_at: now,
            duration_ms: Some(15),
            exit_code: None,
            agent_response_json: None,
            state: JobRunState::Failed,
            error_code: Some("invalid_input".to_string()),
            error_message: error,
        },
    )
    .unwrap();
    jobs.finalize_job_run(&run.run_id, JobRunState::Failed, now, Some(15))
        .unwrap();
    let mut state = runtime.read_run_state(drain).unwrap().unwrap();
    state.record_child_dispatch(ChildDispatch::submitted(
        run.run_id.clone(),
        "task_auto_pipeline".into(),
        "invoke_detached".into(),
        false,
        false,
        now,
    ));
    runtime.write_run_state(drain, &state).unwrap();
    run.run_id.to_string()
}

#[test]
fn a_load_failed_leaf_is_not_readmitted_by_the_same_drain() {
    const TEST: &str =
        "dispatch_admission::load_failure::a_load_failed_leaf_is_not_readmitted_by_the_same_drain";
    if !isolated(TEST) {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    let first = seed(&runtime, Seed::named("load fails a"));
    let second = seed(&runtime, Seed::named("load fails b"));
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    // Before any leaf runs, the drain admits both tasks: this is the wave
    // whose leaves then fail at load.
    let wave = classify(&runtime, &drain);
    assert_eq!(
        wave["loose_task_ids"],
        json!([first.id, second.id]),
        "{wave}"
    );
    let leaf_a = failed_leaf(&runtime, &drain, &first.id, true);
    let leaf_b = failed_leaf(&runtime, &drain, &second.id, true);

    // Repeated passes of the same drain dispatch neither task again: on
    // the base each pass re-dispatched both (44 times per task in the
    // incident window).
    for _ in 0..3 {
        let pass = classify(&runtime, &drain);
        assert_eq!(pass["loose_task_ids"], json!([]), "{pass}");
        assert_eq!(pass["has_leaves"], false, "{pass}");
        let holds = pass["leaf_load_failed_holds"].as_array().unwrap();
        assert_eq!(holds.len(), 2, "{pass}");
        for (task, leaf) in [(&first, &leaf_a), (&second, &leaf_b)] {
            let hold = holds
                .iter()
                .find(|hold| hold["task_id"] == task.id)
                .unwrap_or_else(|| panic!("hold for {}: {pass}", task.id));
            assert_eq!(hold["leaf_run_id"], leaf.as_str(), "{pass}");
            assert!(
                hold["error"].as_str().unwrap().contains("does not resolve"),
                "{pass}"
            );
        }
    }
    // The durable pass record names the held tasks and the load error.
    let recorded = last_pass(&runtime, &drain);
    let held: Vec<_> = recorded
        .deferred
        .iter()
        .filter(|entry| entry.reason.as_deref() == Some("leaf_load_failed"))
        .collect();
    assert_eq!(held.len(), 2, "{recorded:?}");
    assert!(
        held.iter().all(|entry| entry
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("failed at job load")
                && detail.contains("does not resolve"))),
        "{recorded:?}"
    );

    // A task that changes after its leaf failed is admitted again.
    runtime
        .update_task_as_human(
            &first.id,
            orbit_core::application::task::TaskUpdateParams {
                title: Some("load fails a (retried)".to_string()),
                ..Default::default()
            },
            "fixture operator".into(),
        )
        .unwrap();
    let pass = classify(&runtime, &drain);
    assert_eq!(pass["loose_task_ids"], json!([first.id]), "{pass}");
    assert_eq!(
        pass["leaf_load_failed_holds"].as_array().unwrap().len(),
        1,
        "{pass}"
    );

    // A new drain has no load-failure history of its own and admits both.
    let fresh = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let pass = classify(&runtime, &fresh);
    assert!(
        pass["loose_task_ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == &second.id),
        "{pass}"
    );
}

#[test]
fn a_leaf_that_started_steps_is_readmitted_by_the_same_drain() {
    const TEST: &str = "dispatch_admission::load_failure::a_leaf_that_started_steps_is_readmitted_by_the_same_drain";
    if !isolated(TEST) {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    let task = seed(&runtime, Seed::named("started then failed"));
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    failed_leaf(&runtime, &drain, &task.id, false);

    let pass = classify(&runtime, &drain);
    assert_eq!(pass["loose_task_ids"], json!([task.id]), "{pass}");
    assert_eq!(pass["leaf_load_failed_holds"], json!([]), "{pass}");
}
