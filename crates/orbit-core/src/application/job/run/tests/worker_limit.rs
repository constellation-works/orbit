//! [ORB-11253] Retuning a live drain's worker ceiling.

use orbit_common::OrbitError;
use orbit_store::contracts::AuditEventFilter;
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::json;

use super::*;
use crate::application::job::DrainWorkerLimitRequest;

const DRAIN_JOB: &str = "workspace_auto_pipeline";

/// A drain that is running and has checkpointed state, which is the only shape
/// the control can address: the ceiling lives on the run's pipeline state.
fn running_drain(runtime: &OrbitRuntime, submitted: Option<u32>) -> JobRun {
    let input = submitted.map(|submitted| json!({ "max_active_leaf_runs": submitted }));
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(DRAIN_JOB, 1, Utc::now(), input, None)
        .expect("insert drain run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start drain run");
    let state = PipelineState::new(
        run.run_id.clone(),
        DRAIN_JOB.to_string(),
        run.input.clone().unwrap_or_else(|| json!({})),
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &state)
        .expect("write drain state");
    runtime.show_job_run(&run.run_id).expect("reload drain run")
}

fn request<'a>(run_id: &'a str, concurrency: u32) -> DrainWorkerLimitRequest<'a> {
    DrainWorkerLimitRequest {
        run_id,
        max_active_leaf_runs: concurrency,
        expected_revision: None,
        reason: None,
        actor: "tester",
        source: "unit",
        claim_token: None,
    }
}

fn worker_limit_audits(runtime: &OrbitRuntime, run_id: &str) -> Vec<serde_json::Value> {
    runtime
        .list_audit_events_filtered(&AuditEventFilter {
            job_run_id: Some(run_id.to_string()),
            limit: 50,
            ..AuditEventFilter::default()
        })
        .expect("list audits")
        .into_iter()
        .filter(|event| {
            event
                .tool_name
                .as_deref()
                .is_some_and(|tool| tool.starts_with("pipeline.run.workers."))
        })
        .filter_map(|event| {
            let mut value: serde_json::Value = event
                .arguments_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())?;
            value["tool_name"] = json!(event.tool_name);
            Some(value)
        })
        .collect()
}

#[test]
fn raising_the_ceiling_preserves_the_run_and_records_what_changed() {
    let (_root, runtime) = test_runtime();
    let run = running_drain(&runtime, Some(5));

    let change = runtime
        .set_drain_worker_limit(request(&run.run_id, 7))
        .expect("raise ceiling");

    assert_eq!(change.outcome, "updated");
    assert_eq!(change.previous_max_active_leaf_runs, 5);
    assert_eq!(change.max_active_leaf_runs, 7);
    assert_eq!(change.revision, 1);
    assert_eq!(change.job_id, DRAIN_JOB);

    // The run itself is untouched: same id, same state, same input.
    let reloaded = runtime.show_job_run(&run.run_id).expect("reload run");
    assert_eq!(reloaded.run_id, run.run_id);
    assert_eq!(reloaded.state, JobRunState::Running);
    assert_eq!(reloaded.input, run.input);

    let stored = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    let limit = stored.drain_worker_limit.expect("limit recorded");
    assert_eq!(limit.max_active_leaf_runs, 7);
    assert_eq!(limit.actor, "tester");
}

#[test]
fn re_issuing_the_effective_ceiling_is_a_no_op_that_keeps_the_revision() {
    let (_root, runtime) = test_runtime();
    let run = running_drain(&runtime, Some(5));

    let unchanged = runtime
        .set_drain_worker_limit(request(&run.run_id, 5))
        .expect("re-issue submitted ceiling");
    assert_eq!(unchanged.outcome, "unchanged");
    assert_eq!(unchanged.revision, 0);

    runtime
        .set_drain_worker_limit(request(&run.run_id, 7))
        .expect("raise ceiling");
    let repeated = runtime
        .set_drain_worker_limit(request(&run.run_id, 7))
        .expect("re-issue current ceiling");
    assert_eq!(repeated.outcome, "unchanged");
    assert_eq!(repeated.revision, 1);
}

#[test]
fn a_stale_revision_is_refused_as_a_conflict_and_writes_nothing() {
    let (_root, runtime) = test_runtime();
    let run = running_drain(&runtime, Some(5));
    runtime
        .set_drain_worker_limit(request(&run.run_id, 7))
        .expect("first operator wins");

    // A second operator that read revision 0 before the first landed.
    let error = runtime
        .set_drain_worker_limit(DrainWorkerLimitRequest {
            expected_revision: Some(0),
            ..request(&run.run_id, 2)
        })
        .expect_err("stale revision is refused");

    assert!(
        matches!(error, OrbitError::JobRunControlConflict(_)),
        "expected a control conflict, got {error:?}"
    );
    let stored = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    assert_eq!(stored.effective_max_active_leaf_runs(5), 7);
    assert_eq!(stored.drain_worker_limit_revision(), 1);
}

#[test]
fn a_terminal_run_is_refused_and_audited() {
    let (_root, runtime) = test_runtime();
    let run = running_drain(&runtime, Some(5));
    runtime
        .cancel_job_run_with_context(&run.run_id, "tester", "unit")
        .expect("cancel drain");

    let error = runtime
        .set_drain_worker_limit(request(&run.run_id, 7))
        .expect_err("terminal run is refused");

    assert!(
        matches!(error, OrbitError::JobValidation(_)),
        "expected a job validation error, got {error:?}"
    );
    let stored = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    assert!(stored.drain_worker_limit.is_none());
    let rejected = worker_limit_audits(&runtime, &run.run_id)
        .into_iter()
        .find(|event| event["outcome"] == "rejected")
        .expect("rejection is audited");
    assert_eq!(rejected["requested_max_active_leaf_runs"], 7);
}

#[test]
fn an_over_limit_or_zero_ceiling_is_refused_before_any_write() {
    let (_root, runtime) = test_runtime();
    let run = running_drain(&runtime, Some(5));
    let hard_limit = runtime.leaf_run_hard_limit().expect("hard limit");

    for requested in [0, hard_limit + 1] {
        let error = runtime
            .set_drain_worker_limit(request(&run.run_id, requested))
            .expect_err("out-of-range ceiling is refused");
        assert!(
            matches!(error, OrbitError::InvalidInput(_)),
            "expected invalid input for {requested}, got {error:?}"
        );
    }
    let stored = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    assert!(stored.drain_worker_limit.is_none());
}

#[test]
fn a_run_of_another_job_and_a_missing_run_are_refused_distinctly() {
    let (_root, runtime) = test_runtime();
    let leaf = insert_pending_run(&runtime, "task_auto_pipeline");

    let wrong_job = runtime
        .set_drain_worker_limit(request(&leaf.run_id, 7))
        .expect_err("a leaf run has no drain ceiling");
    assert!(
        matches!(wrong_job, OrbitError::InvalidInput(_)),
        "expected invalid input, got {wrong_job:?}"
    );

    let missing = runtime
        .set_drain_worker_limit(request("jrun-missing", 7))
        .expect_err("missing run is refused");
    assert!(
        matches!(missing, OrbitError::NotFound { .. }),
        "expected not found, got {missing:?}"
    );
}

#[test]
fn a_drain_that_has_not_started_is_refused_rather_than_silently_ignored() {
    let (_root, runtime) = test_runtime();
    let pending = insert_pending_run(&runtime, DRAIN_JOB);

    let error = runtime
        .set_drain_worker_limit(request(&pending.run_id, 7))
        .expect_err("a run without checkpoint state cannot carry the control");

    assert!(
        matches!(error, OrbitError::JobValidation(_)),
        "expected a job validation error, got {error:?}"
    );
}

#[test]
fn a_drain_submitted_without_an_explicit_ceiling_reports_the_job_default() {
    let (_root, runtime) = test_runtime();
    let run = running_drain(&runtime, None);

    let change = runtime
        .set_drain_worker_limit(request(&run.run_id, 7))
        .expect("raise ceiling");

    assert_eq!(change.previous_max_active_leaf_runs, 5);
    assert_eq!(change.max_active_leaf_runs, 7);
}

#[test]
fn every_accepted_change_is_audited_with_its_request() {
    let (_root, runtime) = test_runtime();
    let run = running_drain(&runtime, Some(5));

    runtime
        .set_drain_worker_limit(DrainWorkerLimitRequest {
            reason: Some("more headroom"),
            ..request(&run.run_id, 7)
        })
        .expect("raise ceiling");

    let audits = worker_limit_audits(&runtime, &run.run_id);
    let requested = audits
        .iter()
        .find(|event| event["tool_name"] == "pipeline.run.workers.requested")
        .expect("request audited");
    assert_eq!(requested["requested_max_active_leaf_runs"], 7);
    assert_eq!(requested["reason"], "more headroom");
    assert_eq!(requested["source"], "unit");
    let completed = audits
        .iter()
        .find(|event| event["tool_name"] == "pipeline.run.workers.completed")
        .expect("completion audited");
    assert_eq!(completed["outcome"], "updated");
    assert_eq!(completed["previous_max_active_leaf_runs"], 5);
    assert_eq!(completed["max_active_leaf_runs"], 7);
    assert_eq!(completed["revision"], 1);
}
