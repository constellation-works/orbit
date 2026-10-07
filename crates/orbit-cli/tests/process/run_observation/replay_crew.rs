//! Foreground replay exercises the built CLI, with a deterministic catalog job
//! in place of agent work so the persisted admission is observable offline.

use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::{
    PROVIDER_FAILURE_HOLD_EVENT, ProviderFailureClass, ProviderFailureHold,
};
use rusqlite::params;
use serde_json::{Value, json};

use super::{FAILED, Fixture, isolated_run_observation};

fn config(pool: &[&str], default: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"{default}\"\nmedium_complexity_crews = {}\n\
         [crews.held]\nenabled = true\nprovider = \"codex\"\nmodel = \"held-model\"\nbackend = \"cli\"\n\
         [crews.replacement]\nenabled = true\nprovider = \"codex\"\nmodel = \"replacement-model\"\nbackend = \"cli\"\n\
         [review]\nbefore_pr = false\n",
        json!(pool)
    )
}

#[test]
fn replay_recaptures_automatic_crews_and_preserves_explicit_choices() {
    if !isolated_run_observation(
        "run_observation::replay_crew::replay_recaptures_automatic_crews_and_preserves_explicit_choices",
    ) {
        return;
    }
    let fixture = Fixture::init();
    for (selection_source, pool, held) in [
        (Some("pool:medium"), vec!["held", "replacement"], true),
        (Some("default"), vec![], true),
        (Some("explicit"), vec!["held", "replacement"], true),
        (None, vec!["held", "replacement"], true),
        (Some("pool:medium"), vec!["replacement"], false),
    ] {
        let global = fixture.home.join(".orbit");
        let workspace = fixture.work.join(".orbit");
        let config_path = workspace.join("config.toml");
        let original_pool = if selection_source == Some("default") {
            vec![]
        } else {
            vec!["held"]
        };
        std::fs::write(&config_path, config(&original_pool, "held")).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let task = runtime
            .add_task(TaskAddParams {
                title: "Replay crew admission fixture".into(),
                complexity: TaskComplexity::Medium,
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(task.crew.as_deref(), Some("held"));
        let mut input = json!({
            "task_ids": [task.id], "crew": "held", "seconds": 0,
            "allowed_crews": ["held", "replacement"],
            "low_complexity_crews": ["replacement"],
            "auto_crew_pools": {"medium": {"crews": ["held"], "source": "old-policy"}},
        });
        if let Some(source) = selection_source {
            input["crew_selection"] = json!({
                "task_id": task.id, "crew": "held", "source": source,
                "eligible_pool": [{"name": "held", "weight": 1}],
            });
        }
        fixture
            .db()
            .execute(
                "UPDATE job_runs SET job_id='task_auto_pipeline', input_json=?1 WHERE run_id=?2",
                params![input.to_string(), FAILED],
            )
            .unwrap();
        if held {
            let hold = ProviderFailureHold {
                class: ProviderFailureClass::Unavailable,
                provider: Some("codex".into()),
                excluded_crews: vec!["held".into()],
                not_before: Utc::now() + Duration::hours(1),
                run_id: FAILED.into(),
            };
            runtime
                .apply_task_automation_update(
                    &task.id,
                    TaskAutomationUpdate {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                )
                .unwrap();
            runtime
                .apply_task_automation_update(
                    &task.id,
                    TaskAutomationUpdate {
                        status: Some(TaskStatus::Backlog),
                        status_event: Some(PROVIDER_FAILURE_HOLD_EVENT.into()),
                        status_note: Some(hold.text("fixture provider outage")),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        std::fs::write(&config_path, config(&pool, "replacement")).unwrap();
        std::fs::write(
            global.join("resources/jobs/task_auto_pipeline.yaml"),
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_auto_pipeline\nspec:\n  state: enabled\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
        ).unwrap();
        let before = runtime.show_job_run(FAILED).unwrap();
        let replay = fixture.json(&["job", "replay", FAILED, "--json"]);
        assert_eq!(replay["success"], true);
        let id = replay["run_id"].as_str().unwrap();
        let persisted: String = fixture
            .db()
            .query_row(
                "SELECT input_json FROM job_runs WHERE run_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        let admitted: Value = serde_json::from_str(&persisted).unwrap();
        let expected = if selection_source == Some("explicit") {
            "held"
        } else {
            "replacement"
        };
        assert_eq!(admitted["crew"], expected, "source={selection_source:?}");
        assert_eq!(admitted["crew_selection"]["crew"], expected);
        assert_eq!(
            admitted["crew_selection"]["eligible_pool"],
            json!([{"name": expected, "weight": 1}])
        );
        assert_eq!(
            admitted["crew_selection"]["source"] == "explicit",
            selection_source == Some("explicit")
        );
        if held && expected == "replacement" {
            assert!(
                admitted["crew_selection"]["source"]
                    .as_str()
                    .unwrap()
                    .contains("provider hold")
            );
        }
        assert_eq!(
            admitted["auto_crew_pools"]["medium"]["source"],
            "workflow.medium_complexity_crews"
        );
        assert_eq!(
            admitted["auto_crew_pools"]["medium"]["crews"],
            json!(
                pool.iter()
                    .map(|name| json!({"name": name, "weight": 1}))
                    .collect::<Vec<Value>>()
            )
        );
        assert_eq!(
            admitted["auto_crew_pools"]["low"]["crews"],
            json!([{"name": "replacement", "weight": 1}])
        );
        assert_eq!(admitted["allowed_crews"], input["allowed_crews"]);
        assert_eq!(admitted["seconds"], input["seconds"]);
        let shown = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
        assert_eq!(shown["run"]["retry_source_run_id"], FAILED);
        assert_eq!(runtime.show_job_run(FAILED).unwrap(), before);
    }
}
