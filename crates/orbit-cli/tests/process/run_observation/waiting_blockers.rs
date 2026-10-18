//! Waiting-task diagnostics name the blockers without changing JSON records.

use super::*;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{TaskComplexity, TaskStatus, TaskType};
use orbit_types::workflow::{DrainAdmissionPass, PipelineState};
use serde_json::json;

#[test]
fn readiness_and_run_show_name_lock_holders_and_unfinished_dependencies() {
    if !isolated_run_observation(
        "run_observation::waiting_blockers::readiness_and_run_show_name_lock_holders_and_unfinished_dependencies",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    fixture
        .orbit()
        .args([
            "config",
            "set",
            "workflow.resource_throttle.enabled",
            "false",
            "--global",
        ])
        .assert()
        .success();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    for file in ["first.rs", "second.rs", "third.rs", "dependency.rs"] {
        fs::write(fixture.work.join(file), "fixture\n").unwrap();
    }
    let add =
        |title: &str, status, files: &[&str], dependencies: Vec<orbit_types::identity::OrbitId>| {
            runtime
                .add_task(TaskAddParams {
                    title: title.into(),
                    description: "Waiting task observation fixture".into(),
                    acceptance_criteria: vec!["Blocker is observable".into()],
                    plan: "Fixture plan".into(),
                    status: Some(status),
                    task_type: Some(TaskType::Feature),
                    complexity: TaskComplexity::Medium,
                    context_files: files.iter().map(|file| format!("file:{file}")).collect(),
                    dependencies,
                    ..Default::default()
                })
                .unwrap()
                .id
        };
    let holder = add(
        "holder",
        TaskStatus::InProgress,
        &["first.rs", "second.rs"],
        vec![],
    );
    let other_holder = add(
        "other holder",
        TaskStatus::InProgress,
        &["third.rs"],
        vec![],
    );
    let locked = add(
        "locked",
        TaskStatus::Backlog,
        &["first.rs", "second.rs", "third.rs"],
        vec![],
    );
    let dependency = add(
        "dependency",
        TaskStatus::Backlog,
        &["dependency.rs"],
        vec![],
    );
    let dependent = add(
        "dependent",
        TaskStatus::Backlog,
        &["dependency.rs"],
        vec![dependency.clone()],
    );
    let before = fixture.json(&["run", "readiness", "--json"]);
    let task = |id: &str| {
        before["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["task_id"] == id)
            .unwrap()
    };
    let conflicts = task(locked.as_str())["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 3, "{before:#}");
    assert_eq!(
        conflicts[0],
        json!({"requested_file": "file:first.rs", "locking_task_id": holder})
    );
    assert_eq!(task(dependent.as_str())["reason"], "unmet_dependency");
    assert_eq!(
        task(dependent.as_str())["dependencies"],
        json!([{"task_id": dependency, "status": "backlog"}])
    );
    let text = |args: &[&str]| {
        let output = fixture.orbit().args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let ready = text(&["run", "readiness"]);
    let lock_detail = format!(
        "blocked-by={holder} (in-progress) on file:first.rs +1 more, {other_holder} (in-progress) on file:third.rs"
    );
    let dependency_detail = format!("waits-on {dependency} (backlog)");
    assert!(
        ready.contains(&format!("{locked}: waiting (context_lock_conflict)")),
        "{ready}"
    );
    assert!(
        ready.contains(&lock_detail),
        "missing lock holder/status/files: {ready}"
    );
    assert!(
        ready.contains(&format!("{dependent}: waiting (unmet_dependency)")),
        "{ready}"
    );
    assert!(
        ready.contains(&dependency_detail),
        "missing dependency/status: {ready}"
    );
    let after = fixture.json(&["run", "readiness", "--json"]);
    assert_eq!(
        after["tasks"], before["tasks"],
        "text rendering must preserve readiness JSON"
    );

    // Last-pass records intentionally carry IDs only. Both local and pull
    // drains use the same reason and blocker wording with available statuses.
    for job in ["workspace_auto_pipeline", "workspace_pull_pipeline"] {
        let id = format!("jrun-cli-waiting-{job}");
        let now = chrono::Utc::now();
        fixture.db().execute(
            "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,?3,1,'running','{}',?4,?4,?4,?5)",
            params![id, fixture.workspace_id(), job, now.to_rfc3339(), std::process::id()],
        ).unwrap();
        let pass = json!({
            "recorded_at": now, "queued": 1,
            "deferred": [{"task_id": locked, "reason": "context_lock_conflict", "blocked_by": [holder, other_holder]}],
            "excluded": [{"task_id": dependent, "reason": "dependency_not_done", "blocked_by": [dependency]}],
            "excluded_total": 1
        });
        let mut state = PipelineState::new(id.clone(), job.into(), json!({}));
        state.pull_drain.drain_last_pass =
            Some(serde_json::from_value::<DrainAdmissionPass>(pass).unwrap());
        runtime.write_run_state(&id, &state).unwrap();
        let before = fixture.json(&["run", "show", &id, "--no-reconcile", "--json"]);
        let shown = text(&["run", "show", &id, "--no-reconcile"]);
        assert!(shown.contains(&format!("Task {locked}: context_lock_conflict blocked-by={holder} (in-progress), {other_holder} (in-progress)")), "{shown}");
        assert!(
            shown.contains(&format!(
                "Task {dependent}: unmet_dependency {dependency_detail}"
            )),
            "{shown}"
        );
        let after = fixture.json(&["run", "show", &id, "--no-reconcile", "--json"]);
        assert_eq!(after["drain_summary"], before["drain_summary"]);
        assert_eq!(after["pipeline_state"], before["pipeline_state"]);
        assert_eq!(
            after["pipeline_state"]["drain_last_pass"]["excluded"][0]["reason"],
            "dependency_not_done"
        );
    }
}
