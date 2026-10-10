//! Dashboard/API replay admission must recapture crew policy, while retaining
//! the operator's explicit choice. Workers are harmless substitutes: admission
//! is complete before the new run is persisted.

use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::{
    JobRunState, JobRunTrigger, PROVIDER_FAILURE_HOLD_EVENT, ProviderFailureClass,
    ProviderFailureHold,
};
use serde_json::{Value, json};

fn config(pool: &[&str], default: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"{default}\"\nmedium_complexity_crews = {}\n\
         [crews.held]\nprovider = \"codex\"\nmodel = \"held-model\"\n\
         [crews.replacement]\nprovider = \"codex\"\nmodel = \"replacement-model\"\n\
         [review]\nbefore_pr = false\n",
        json!(pool)
    )
}

#[test]
fn detached_replay_recaptures_automatic_crews_and_preserves_explicit_choices() {
    if !super::isolated(
        "dispatch_admission::replay_crew::detached_replay_recaptures_automatic_crews_and_preserves_explicit_choices",
    ) {
        return;
    }
    orbit_core::test_support::install_substitute_pipeline_worker([
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--list".into(),
    ]);
    for (selection_source, pool, held) in [
        (Some("pool:medium"), vec!["held", "replacement"], true),
        (Some("default"), vec![], true),
        (Some("explicit"), vec!["held", "replacement"], true),
        (None, vec!["held", "replacement"], true),
        (Some("pool:medium"), vec!["replacement"], false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
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
            "task_ids": [task.id], "crew": "held", "marker": "retained",
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
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let source = jobs
            .insert_job_run(
                "task_auto_pipeline",
                1,
                Utc::now(),
                Some(input.clone()),
                None,
            )
            .unwrap();
        jobs.mark_job_run_running(&source.run_id, Utc::now(), std::process::id())
            .unwrap();
        jobs.finalize_job_run(&source.run_id, JobRunState::Failed, Utc::now(), Some(1))
            .unwrap();
        if held {
            let hold = ProviderFailureHold {
                class: ProviderFailureClass::Unavailable,
                provider: Some("codex".into()),
                excluded_crews: vec!["held".into()],
                not_before: Utc::now() + Duration::hours(1),
                run_id: source.run_id.clone(),
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
        // Replace policy after the original draw, then reopen to resolve it.
        std::fs::write(&config_path, config(&pool, "replacement")).unwrap();
        std::fs::write(
            jobs_dir.join("task_auto_pipeline.yaml"),
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_auto_pipeline\nspec:\n  state: enabled\n  steps: []\n",
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let before = runtime.show_job_run(&source.run_id).unwrap();
        let replay = runtime
            .submit_replay_run(&source.run_id, None, None, JobRunTrigger::dashboard())
            .unwrap();
        let replay = jobs.get_job_run(&replay.run_id).unwrap().unwrap();
        let admitted = replay.input.unwrap();
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
            selection_source == Some("explicit"),
            "replay must retain selection provenance"
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
        assert_eq!(admitted["marker"], input["marker"]);
        assert_eq!(
            replay.retry_source_run_id.as_deref(),
            Some(source.run_id.as_str())
        );
        assert_eq!(runtime.show_job_run(&source.run_id).unwrap(), before);
    }
}
