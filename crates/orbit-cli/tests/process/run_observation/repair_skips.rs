//! A persisted repair refusal reaches both CLI observation surfaces.

use super::*;
use orbit_types::workflow::PipelineState;
use serde_json::json;

#[test]
fn readiness_and_run_show_expose_the_last_pass_repair_refusal() {
    if !isolated_run_observation(
        "run_observation::repair_skips::readiness_and_run_show_expose_the_last_pass_repair_refusal",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let run = "jrun-repair-skips";
    let now = chrono::Utc::now().to_rfc3339();
    fixture
        .db()
        .execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
            scheduled_at, started_at, finished_at, created_at)
         VALUES (?1, ?2, 'workspace_auto_pipeline', 1, 'success', ?3, ?3, ?3, ?3)",
            params![run, fixture.workspace_id(), now],
        )
        .unwrap();
    fixture
        .db()
        .execute(
            "INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type,
            target_id, state, started_at, finished_at)
         VALUES (?1, ?2, 0, 'activity', 'adopt_orphans', 'success', ?3, ?3)",
            params![fixture.workspace_id(), run, now],
        )
        .unwrap();
    let pass = json!({"observed_at": now, "auto_fix": {"started": [], "skipped": [{
        "task_id": "TASK-123", "source_run_id": "jrun-source", "pr_number": "81",
        "blocker": "conflict", "reason": "not_resumable", "detail": "completion was never reached"
    }]}});
    let mut state = PipelineState::new(run.into(), "workspace_auto_pipeline".into(), json!({}));
    state.record_pipeline_output("adopt_orphans", pass.clone());
    runtime.write_run_state(run, &state).unwrap();

    let shown = fixture.json(&[
        "run",
        "show",
        run,
        "--step",
        "adopt_orphans",
        "--json",
        "--no-reconcile",
    ]);
    assert_eq!(shown["step_output"], pass);
    let readiness = fixture.json(&["run", "readiness", "--json"]);
    let skipped = &readiness["out_of_run_auto_fix"][0];
    assert_eq!(skipped["pr_number"], "81");
    assert_eq!(skipped["reason"], "not_resumable");
    assert_eq!(skipped["state"], "skipped");
    assert_eq!(skipped["drain_live"], false);
    let output = fixture.orbit().args(["run", "readiness"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .find(|line| line.contains("PR #81"))
        .expect("readiness names the refused PR");
    assert!(
        line.contains("not_resumable")
            && line.contains("conflict")
            && line.contains("completion was never reached"),
        "{line}"
    );
    assert!(
        line.contains("ended drain"),
        "a completed pass must not look live: {line}"
    );
}
