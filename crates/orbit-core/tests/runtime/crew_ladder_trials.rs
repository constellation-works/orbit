//! Crew-ladder trials survive a runtime opened before the operator's config
//! change: admissions from a stale mode-off runtime and a current shadow
//! runtime share one trial, while a real operator change is still honored.

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput, ExperimentView,
};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{TaskComplexity, TaskStatus, TaskType};
use serde_json::json;

const LADDER_ACTOR: &str = "system:crew-ladder";

fn config(mode: &str, explore_rate: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"sol\"\nhard_complexity_crews = [\"sol\"]\n\n\
         [workflow.crew_ladder]\nmode = \"{mode}\"\nrungs = [\"terra\", \"sol\"]\n\
         tiers = [\"hard\"]\nexplore_rate = {explore_rate}\n\n\
         [crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
         [crews.terra]\nprovider = \"codex\"\nmodel = \"test-small\"\n\n\
         [experiments]\nenabled = true\n"
    )
}

fn ladder_trials(runtime: &OrbitRuntime) -> Vec<ExperimentView> {
    runtime
        .experiment_views(None)
        .unwrap()
        .into_iter()
        .filter(|view| view.created_by == LADDER_ACTOR)
        .collect()
}

/// Admit one fresh hard task through `runtime`, as a drain does.
fn admit_hard_task(runtime: &OrbitRuntime, job_path: &std::path::Path, title: &str) {
    let task = runtime
        .add_task(TaskAddParams {
            title: title.into(),
            complexity: TaskComplexity::Hard,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .unwrap();
    runtime
        .submit_job_run(
            job_path.to_str().unwrap(),
            json!({ "task_id": task.id, "crew": "sol" }),
            Some("crew-ladder-trials-test"),
        )
        .unwrap();
}

fn job_path(root: &std::path::Path) -> std::path::PathBuf {
    let job_path = root.join("task_local_pipeline.yaml");
    std::fs::write(
        &job_path,
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_local_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    )
    .unwrap();
    orbit_core::test_support::install_substitute_pipeline_worker([
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--list".to_string(),
    ]);
    job_path
}

#[test]
fn stale_mode_off_runtime_keeps_the_current_ladder_trial() -> Result<(), OrbitError> {
    if !super::dispatch_admission::isolated(
        "crew_ladder_trials::stale_mode_off_runtime_keeps_the_current_ladder_trial",
    ) {
        return Ok(());
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config_path = workspace.join("config.toml");
    std::fs::write(&config_path, config("off", "0.1")).unwrap();
    let job = job_path(root.path());

    // The long-lived owner runtime opened while the ladder was off.
    let stale = OrbitRuntime::from_roots(&global, &workspace)?;
    // This second runtime has never refreshed since opening with mode off.
    let never_refreshed = OrbitRuntime::from_roots(&global, &workspace)?;
    admit_hard_task(&stale, &job, "admitted while the ladder is off");
    assert!(ladder_trials(&stale).is_empty());

    // The operator opts in; a runtime opened afterwards sees shadow.
    std::fs::write(&config_path, config("shadow", "0.1")).unwrap();
    let current = OrbitRuntime::from_roots(&global, &workspace)?;
    let operator = ExperimentActor {
        actor: "crew-ladder-trials-test".into(),
        reason: None,
    };
    let operator_experiment = current.create_experiment(
        "operator-study",
        ExperimentDefinitionInput {
            arms: Some(json!([
                { "name": "control", "control": true },
                { "name": "treatment" }
            ])),
            population: Some(json!({ "tags_include": ["operator-study"] })),
            primary_metric: Some("total_tokens".into()),
            ..ExperimentDefinitionInput::default()
        },
        operator.clone(),
    )?;
    current.change_experiment(&operator_experiment.id, ExperimentChange::Start, operator)?;
    admit_hard_task(&current, &job, "first shadow admission");
    let trials = ladder_trials(&current);
    assert_eq!(trials.len(), 1, "{trials:?}");
    let trial = trials[0].clone();
    assert_eq!(trial.status, "running");
    let events = current.experiment_view(&trial.id)?.events.len();

    // A failed first reload must not reconcile the shared store using the
    // old runtime's unrelated, open-time off fallback.
    std::fs::write(&config_path, "[workflow.crew_ladder\nmode = ").unwrap();
    admit_hard_task(&never_refreshed, &job, "first reload is invalid");
    let after_failed_first_reload = ladder_trials(&current);
    assert_eq!(after_failed_first_reload.len(), 1);
    assert_eq!(after_failed_first_reload[0].id, trial.id);
    assert_eq!(after_failed_first_reload[0].status, "running");
    assert_eq!(current.experiment_view(&trial.id)?.events.len(), events);
    std::fs::write(&config_path, config("shadow", "0.1")).unwrap();

    // Alternate real admissions across the stale and the current runtime.
    for round in 0..3 {
        admit_hard_task(&stale, &job, &format!("stale admission {round}"));
        admit_hard_task(&current, &job, &format!("current admission {round}"));
    }
    let after = ladder_trials(&current);
    assert_eq!(after.len(), 1, "the trial was recreated: {after:?}");
    assert_eq!(after[0].id, trial.id);
    assert_eq!(after[0].status, "running");
    assert_eq!(current.experiment_view(&trial.id)?.events.len(), events);

    // A config the loader refuses keeps the last-known-good ladder.
    std::fs::write(&config_path, "[workflow.crew_ladder\nmode = ").unwrap();
    admit_hard_task(&stale, &job, "admitted under invalid config");
    admit_hard_task(&current, &job, "admitted under invalid config, current");
    assert_eq!(
        ladder_trials(&current)
            .iter()
            .map(|view| (view.id.as_str(), view.status.as_str()))
            .collect::<Vec<_>>(),
        vec![(trial.id.as_str(), "running")]
    );

    // A genuine definition change replaces the trial, from either runtime.
    std::fs::write(&config_path, config("shadow", "0.2")).unwrap();
    admit_hard_task(&stale, &job, "admitted after an explore_rate change");
    let replaced = ladder_trials(&current);
    assert_eq!(replaced.len(), 2, "{replaced:?}");
    assert_eq!(replaced[0].id, trial.id);
    assert_eq!(replaced[0].status, "concluded");
    assert_eq!(replaced[1].status, "running");
    let replacement = replaced[1].id.clone();
    admit_hard_task(&current, &job, "current admission after the change");
    assert_eq!(ladder_trials(&current).len(), 2);

    // A genuine operator disable concludes the trial, from either runtime.
    std::fs::write(&config_path, config("off", "0.2")).unwrap();
    admit_hard_task(&stale, &job, "admitted after the operator disabled it");
    let disabled = ladder_trials(&current);
    assert_eq!(disabled.len(), 2, "{disabled:?}");
    assert!(
        disabled.iter().all(|view| view.status == "concluded"),
        "{disabled:?}"
    );
    assert_eq!(disabled[1].id, replacement);

    // Operator-owned experiments and history are untouched.
    let operator_after = current.experiment_view(&operator_experiment.id)?;
    assert_eq!(operator_after.status, "running");
    assert_eq!(operator_after.revision, operator_experiment.revision + 1);
    Ok(())
}
