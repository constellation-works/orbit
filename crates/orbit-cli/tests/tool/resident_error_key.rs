use chrono::Utc;
use orbit_core::app::resident;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::runtime::OrbitRuntime;
use orbit_types::task::TaskArtifact;
use orbit_types::workflow::{RESIDENT_ACTION_EVENT, ResidentActionRow};
use serde_json::{Value, json};

#[test]
fn failed_resident_comment_cli_returns_the_recorded_action_key() {
    if crate::isolated_test::run_in_child(
        "resident_error_key::failed_resident_comment_cli_returns_the_recorded_action_key",
    ) {
        return;
    }
    let fx = crate::isolated_cli_fixture::Fixture::new();
    let runtime = OrbitRuntime::from_roots(&fx.root, &fx.repo.join(".orbit")).unwrap();
    let watched = runtime
        .add_task(TaskAddParams {
            title: "Watched work".into(),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let mandate = runtime
        .add_task(TaskAddParams {
            title: "Resident assignment".into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let body = json!({"schema":"resident.mandate/v1", "goal":"Watch work", "exit_criteria":["Done"], "workspaces":[runtime.workspace_id().unwrap()], "watch":{"tasks":[watched],"tags":[]}, "crew":"sol", "cadence":{"floor_minutes":10,"heartbeat_minutes":120}});
    runtime
        .update_task_as_human(
            &mandate,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "resident/mandate.json",
                    body.to_string(),
                )],
                ..Default::default()
            },
            "human:test".into(),
        )
        .unwrap();
    let jobs = runtime.stores().jobs();
    jobs.insert_automation_job_run(
        "resident_cycle_pipeline",
        json!({"task_ids":[mandate]}),
        "resident-error-key",
    )
    .unwrap();
    let runs = jobs.list_job_runs("resident_cycle_pipeline").unwrap();
    let run = &runs[0].run_id;
    let brief = resident::begin_resident_cycle(&runtime, run, Utc::now()).unwrap();
    assert_eq!(brief["cycle_count"], 1, "{brief}");
    let input =
        json!({"id":mandate, "comment":json!({"kind":"resident.request", "schema":1}).to_string()})
            .to_string();
    let output = fx
        .command(&[
            "tool",
            "run",
            "orbit.task.update",
            "--format",
            "json",
            "--input",
            &input,
        ])
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env("ORBIT_RUN_ID", run)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: Value = serde_json::from_str(
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(error["code"], "invalid_input", "{error}");
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("missing field `mandate`"),
        "{error}"
    );
    let rows: Vec<ResidentActionRow> = runtime
        .get_task_history(&mandate)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == RESIDENT_ACTION_EVENT)
        .filter_map(|entry| serde_json::from_str(entry.note.as_deref()?).ok())
        .collect();
    assert_eq!(error["action_key"], rows.last().unwrap().key, "{error}");
    let output = fx
        .command(&[
            "tool",
            "run",
            "orbit.task.update",
            "--format",
            "json",
            "--input",
            "{}",
        ])
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env("ORBIT_RUN_ID", run)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let denied: Value = serde_json::from_str(
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert!(denied.get("action_key").is_none(), "{denied}");
    assert_eq!(
        runtime
            .get_task_history(&mandate)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == RESIDENT_ACTION_EVENT)
            .count(),
        rows.len()
    );
}
