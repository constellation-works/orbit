//! Worktree failures in the public run projection retain their raw details.
use orbit_core::application::job::{job_run_to_json, run_display_steps};
use orbit_core::runtime::audit::run::{AuditStepOutcome, RunAuditStep};
use orbit_types::workflow::{JobRun, JobRunState, JobRunStep};
use serde_json::{Value, json};

#[test]
fn historical_and_audited_worktree_failures_keep_reachable_diagnostics() {
    let diagnostic = json!({
        "code": "worktree_mismatch",
        "reason": "distinct checkouts require a declared, matching workspace_path/repo_root pair",
        "requested_repo_root": null,
        "requested_workspace_path": "/work/candidate",
        "automatic_reconciliation": false,
    });
    let message = format!(
        "execution failed: v2 job dispatch: worktree integrity violation `worktree_mismatch`: {diagnostic}"
    );
    let mut run: JobRun = serde_json::from_value(json!({
        "run_id": "jrun-historical", "job_id": "resident_cycle_pipeline", "attempt": 1,
        "state": "failed", "scheduled_at": "2026-11-15T20:52:00Z", "created_at": "2026-11-15T20:52:00Z"
    })).unwrap();
    let step: JobRunStep = serde_json::from_value(json!({
        "step_index": 0, "target_type": "job", "target_id": "resident_cycle_pipeline",
        "state": "failed", "error_message": message,
    }))
    .unwrap();
    run.steps.push(step);
    let row = job_run_to_json(&run, None);
    let readable = row["error_message"].as_str().unwrap();
    assert!(
        readable.contains("no repo root") && !readable.contains('{'),
        "historical JSON must be separated from the readable failure: {readable}"
    );
    assert_eq!(row["error_diagnostic"], diagnostic);
    assert_eq!(row["steps"][0]["error_diagnostic"], diagnostic);
    // The new audit field also reaches the public detail projection without
    // requiring a schema change to the stored job-run step.
    let displayed = run_display_steps(&run, || {
        vec![RunAuditStep {
            lock_wait_ms: None,
            step_index: 0,
            step_id: "implement".into(),
            started_at: None,
            finished_at: None,
            outcome: Some(AuditStepOutcome::Error),
            error_message: Some(readable.into()),
            error_diagnostic: Some(diagnostic.clone()),
        }]
    });
    assert_eq!(
        displayed.records[0].to_json()["error_diagnostic"],
        diagnostic
    );
    assert_eq!(displayed.records[0].to_json()["state"], "error");
    // Historical events with no diagnostic still project normally.
    run.state = JobRunState::Success;
    assert_eq!(job_run_to_json(&run, None)["error_diagnostic"], Value::Null);
}
