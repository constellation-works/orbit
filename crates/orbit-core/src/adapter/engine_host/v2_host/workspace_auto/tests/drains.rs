use chrono::Utc;
use serde_json::{Value, json};

use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, write_workspace_file,
};

use super::support::{
    classify_with, readiness, seed_backlog_leaves, seed_running_drain, seed_running_drain_input,
    set_worker_limit,
};

#[test]
fn readiness_reports_the_live_ceiling_and_who_moved_it() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/leaf_0/src/lib.rs");
    let backlog = seed_backlog_leaves(&runtime, 1);
    let drain_run_id = seed_running_drain(&runtime, 5);

    let submitted = readiness(&runtime, &backlog, None);
    assert_eq!(submitted["capacity"]["max_active_leaf_runs"], 5);
    assert_eq!(submitted["capacity"]["limit_source"], "run_input");
    assert_eq!(submitted["capacity"]["drain_run_id"], drain_run_id);

    set_worker_limit(&runtime, &drain_run_id, 7);

    let adjusted = readiness(&runtime, &backlog, None);
    assert_eq!(adjusted["capacity"]["max_active_leaf_runs"], 7);
    assert_eq!(adjusted["capacity"]["limit_source"], "run_control");
    assert_eq!(adjusted["capacity"]["worker_limit"]["actor"], "tester");
    assert_eq!(adjusted["capacity"]["worker_limit"]["revision"], 1);

    // An explicit `--concurrency` still previews what the operator typed.
    let previewed = readiness(&runtime, &backlog, Some(2));
    assert_eq!(previewed["capacity"]["max_active_leaf_runs"], 2);
    assert_eq!(previewed["capacity"]["limit_source"], "requested");
}

/// A replica's pull drain is a coordinator `orbit run auto --stop` acts on, so
/// readiness must show it live and then stopped; it is not the auto drain.
#[test]
fn readiness_reports_a_live_pull_drain_and_its_stopped_admissions() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let idle = readiness(&runtime, &[], None);
    assert_eq!(idle["capacity"]["pull_drain_run_id"], Value::Null);
    assert_eq!(idle["capacity"]["pull_drain_admissions_stopped"], false);

    let input = json!({ "owner_machine_id": "owner-1" });
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            crate::application::distributed::PULL_DRAIN_JOB,
            1,
            Utc::now(),
            Some(input.clone()),
            None,
        )
        .expect("insert pull drain run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start pull drain run");
    runtime
        .stores()
        .jobs()
        .write_run_state(
            &run.run_id,
            &orbit_types::workflow::PipelineState::new(
                run.run_id.clone(),
                crate::application::distributed::PULL_DRAIN_JOB.to_string(),
                input,
            ),
        )
        .expect("write pull drain state");

    let live = readiness(&runtime, &[], None);
    assert_eq!(live["capacity"]["pull_drain_run_id"], run.run_id);
    assert_eq!(live["capacity"]["pull_drain_admissions_stopped"], false);
    assert_eq!(
        live["capacity"]["drain_run_id"],
        Value::Null,
        "the pull drain is not the auto drain: {live}"
    );

    runtime
        .stop_workspace_auto_admissions(crate::application::job::DrainAdmissionsStopRequest {
            actor: "tester",
            source: "unit",
            reason: None,
            claim_token: None,
        })
        .expect("stop drains");

    let stopped = readiness(&runtime, &[], None);
    assert_eq!(stopped["capacity"]["pull_drain_run_id"], run.run_id);
    assert_eq!(stopped["capacity"]["pull_drain_admissions_stopped"], true);
    assert_eq!(
        stopped["capacity"]["pull_drain_admissions_stop"]["actor"],
        "tester"
    );
}

#[test]
fn readiness_separates_a_queued_drain_from_the_running_coordinator() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let running = seed_running_drain_input(
        &runtime,
        json!({ "max_active_leaf_runs": 3, "completion": "done" }),
    );
    let queued = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({ "max_active_leaf_runs": 5, "completion": "review" })),
            None,
        )
        .expect("queue drain");

    let output = readiness(&runtime, &[], None);
    assert_eq!(output["capacity"]["drain_run_id"], running);
    assert_eq!(output["capacity"]["max_active_leaf_runs"], 3);
    assert_eq!(output["capacity"]["limit_source"], "run_input");
    assert_eq!(
        output["capacity"]["queued_drains"],
        json!([{ "run_id": queued.run_id, "max_active_leaf_runs": 5, "completion": "review" }])
    );
}

/// [ORB-11273] `orbit run job ... --input max_active_leaf_runs=7` persists the
/// ceiling as a JSON string. Readiness must report that live drain ceiling
/// (and the same source the classifier uses), not the numeric-only fallback of 5.
#[test]
fn readiness_parses_numeric_and_string_run_input_ceilings() {
    for submitted in [json!(7), json!("7")] {
        let (_root, runtime, repo_root) = runtime_with_workspace_layout();
        write_workspace_file(&repo_root, "crates/leaf_0/src/lib.rs");
        let backlog = seed_backlog_leaves(&runtime, 1);
        let drain_run_id =
            seed_running_drain_input(&runtime, json!({ "max_active_leaf_runs": submitted }));

        let output = readiness(&runtime, &backlog, None);
        assert_eq!(
            output["capacity"]["max_active_leaf_runs"], 7,
            "submitted {submitted} must report the live ceiling, not the default 5"
        );
        assert_eq!(output["capacity"]["limit_source"], "run_input");
        assert_eq!(output["capacity"]["drain_run_id"], drain_run_id);

        let classified = classify_with(
            &runtime,
            json!({ "run_id": drain_run_id, "max_active_leaf_runs": submitted }),
        );
        assert_eq!(classified["max_active_leaf_runs"], 7);
        assert_eq!(classified["submitted_max_active_leaf_runs"], 7);
        assert_eq!(classified["worker_limit_source"], "run_input");
    }
}
