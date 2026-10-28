//! [ORB-19416] `task.crew` names the crew that runs: admission writes back a
//! crew an experiment arm or a provider hold chose, and a task with an
//! explicit crew stays out of crew experiments unless an operator forces it.
//!
//! In ws_orbit the cheap-pool arm of exp-4e3e0399 ran luna, haiku and
//! muse-spark while every enrolled task kept its creation-time crew, ORB-19405
//! ran grok under a codex capacity hold while still naming sol, and the
//! explicitly crewed ORB-19386 ran luna in the control arm.

use orbit_core::OrbitRuntime;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_types::workflow::ProviderFailureClass;
use serde_json::{Value, json};

use super::dispatch_admission::isolated;
use super::provider_failure_hold::{Fixture, fixture};

/// One Codex crew in the medium pool, a second Codex crew the experiment
/// arm runs, and experiments on.
const EXPERIMENT_CREWS: &str = r#"[workflow]
default_crew = "sol"
medium_complexity_crews = ["sol"]

[crews.sol]
provider = "codex"
model = "sol-model"

[crews.luna]
provider = "codex"
model = "luna-model"

[experiments]
enabled = true
"#;

/// Two Codex crews in the medium pool and a Claude default.
const HOLD_CREWS: &str = r#"[workflow]
default_crew = "opus"
medium_complexity_crews = ["sol", "luna"]

[crews.sol]
provider = "codex"
model = "sol-model"

[crews.luna]
provider = "codex"
model = "luna-model"

[crews.opus]
provider = "claude"
model = "opus-model"
"#;

/// The control arm keeps ordinary routing; `cheap-pool` runs luna.
fn started_crew_experiment(runtime: &OrbitRuntime) -> String {
    let actor = ExperimentActor {
        actor: "admitted-crew-test".into(),
        reason: None,
    };
    let experiment = runtime
        .create_experiment(
            "cheap",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true, "weight": 1 },
                    {
                        "name": "cheap-pool",
                        "weight": 1,
                        "overlay": { "crew": { "pool": [{ "crew": "luna", "weight": 1 }] } }
                    }
                ])),
                primary_metric: Some("implement_tokens".into()),
                mode: Some("live".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();
    experiment.id
}

/// A catalog-shaped delivery job whose worker does nothing, so a submission
/// runs admission and stops.
fn delivery_job(fx: &Fixture) -> std::path::PathBuf {
    let job_path = fx.repo.join("task_local_pipeline.yaml");
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

/// Admit `task` through a real submission; returns the run's input.
fn submit(fx: &Fixture, job: &std::path::Path, input: Value) -> Value {
    let submitted = fx
        .runtime
        .submit_job_run(job.to_str().unwrap(), input, Some("admitted-crew-test"))
        .unwrap();
    fx.runtime
        .show_job_run_observed(&submitted.run_id)
        .unwrap()
        .input
        .unwrap()
}

fn crew_of(fx: &Fixture, task: &str) -> (Option<String>, Option<String>) {
    let task = fx.runtime.get_task(task).unwrap();
    (task.crew, task.crew_source)
}

fn last_crew_note(fx: &Fixture, task: &str) -> String {
    fx.runtime
        .get_task_history(task)
        .unwrap()
        .into_iter()
        .rev()
        .find(|entry| entry.event == "crew_assigned")
        .and_then(|entry| entry.note)
        .expect("a crew_assigned entry")
}

/// A treatment arm's crew is written back with its experiment provenance, so
/// the readout's crew mix shows the arm's crew; a control task that ran a
/// crew outside the complexity pool is named by a `crew_mismatch` warning.
#[test]
fn an_experiment_arm_crew_is_written_back_to_the_task_and_read_by_the_readout() {
    if !isolated(
        "admitted_crew_write_back::an_experiment_arm_crew_is_written_back_to_the_task_and_read_by_the_readout",
    ) {
        return;
    }
    let fx = fixture(EXPERIMENT_CREWS);
    let job = delivery_job(&fx);
    let experiment = started_crew_experiment(&fx.runtime);

    // The arm is drawn from the experiment's random salt: enroll tasks until
    // both arms hold one (each draw is a fair coin).
    let (mut treated, mut control) = (None, None);
    for _ in 0..40 {
        let task = fx.tagged_task(&[], None).id;
        assert_eq!(crew_of(&fx, &task).0.as_deref(), Some("sol"));
        let input = submit(&fx, &job, json!({ "task_id": task }));
        let arm = fx.runtime.task_experiments(&task).unwrap()[0].arm.clone();
        match arm.as_deref() {
            Some("cheap-pool") if treated.is_none() => {
                assert_eq!(input["crew_selection"]["crew"], "luna", "{input}");
                treated = Some(task);
            }
            Some("control") if control.is_none() => control = Some(task),
            _ => {}
        }
        if treated.is_some() && control.is_some() {
            break;
        }
    }
    let (treated, control) = (treated.unwrap(), control.unwrap());

    assert_eq!(
        crew_of(&fx, &treated),
        (
            Some("luna".to_string()),
            Some(format!("experiment:{experiment}/cheap-pool"))
        )
    );
    assert_eq!(
        last_crew_note(&fx, &treated),
        format!("crew changed from `sol` to `luna` by experiment {experiment} arm cheap-pool")
    );
    assert_eq!(crew_of(&fx, &control).0.as_deref(), Some("sol"));

    // An explicit run crew outside the control arm's pool runs, and is
    // written back as the run's crew.
    submit(&fx, &job, json!({ "task_id": control, "crew": "luna" }));
    assert_eq!(
        crew_of(&fx, &control),
        (
            Some("luna".to_string()),
            Some("redraw:run_crew".to_string())
        )
    );

    let readout = fx.runtime.experiment_readout(&experiment, None).unwrap();
    let cheap = readout
        .arms
        .iter()
        .find(|arm| arm.name == "cheap-pool")
        .unwrap();
    assert!(
        cheap.crew_mix.iter().all(|row| row.crew == "luna"),
        "{:?}",
        cheap.crew_mix
    );
    let mismatches: Vec<&str> = readout
        .warnings
        .iter()
        .filter(|warning| warning.kind == "crew_mismatch")
        .map(|warning| warning.message.as_str())
        .collect();
    assert_eq!(mismatches.len(), 1, "{mismatches:?}");
    assert!(
        mismatches[0].contains(&format!("task {control} in arm `control` ran crew `luna`")),
        "{mismatches:?}"
    );
}

/// A capacity hold on the task's pool crew moves admission to the other pool
/// member, and the task records that crew with `redraw:provider_hold`.
#[test]
fn a_provider_hold_redraw_is_written_back_to_the_task() {
    if !isolated("admitted_crew_write_back::a_provider_hold_redraw_is_written_back_to_the_task") {
        return;
    }
    let fx = fixture(HOLD_CREWS);
    let job = delivery_job(&fx);
    let created = fx.tagged_task(&[], None);
    assert_eq!(created.crew_source.as_deref(), Some("pool:medium"));
    let held = created.crew.clone().unwrap();
    let other = if held == "sol" { "luna" } else { "sol" };
    let run = fx.admit(&created.id, &held);
    fx.fail(
        &run,
        &format!(
            "step `implement_one`: {}",
            orbit_types::workflow::provider_failure_text(
                ProviderFailureClass::Capacity,
                "codex",
                "codex provider reported the selected model at capacity",
            )
        ),
    );
    assert_eq!(
        fx.last_hold(&created.id).excluded_crews,
        std::slice::from_ref(&held)
    );

    let input = submit(&fx, &job, json!({ "task_id": created.id }));
    assert_eq!(input["crew_selection"]["crew"], other, "{input}");
    assert_eq!(
        crew_of(&fx, &created.id),
        (
            Some(other.to_string()),
            Some("redraw:provider_hold".to_string())
        )
    );
    assert_eq!(
        last_crew_note(&fx, &created.id),
        format!("crew changed from `{held}` to `{other}` at admission (provider_hold)")
    );
}

/// A task with an explicit crew is excluded from an experiment whose arms
/// overlay the crew, and the readout counts it. Forcing it into an arm runs
/// the arm's crew and writes it back.
#[test]
fn an_explicit_crew_is_excluded_from_crew_experiments_until_forced() {
    if !isolated(
        "admitted_crew_write_back::an_explicit_crew_is_excluded_from_crew_experiments_until_forced",
    ) {
        return;
    }
    let fx = fixture(EXPERIMENT_CREWS);
    let job = delivery_job(&fx);
    let experiment = started_crew_experiment(&fx.runtime);
    let task = fx.tagged_task(&[], Some("sol")).id;
    assert_eq!(
        crew_of(&fx, &task),
        (Some("sol".to_string()), Some("explicit".to_string()))
    );

    let input = submit(&fx, &job, json!({ "task_id": task }));
    assert_eq!(input["crew_selection"]["crew"], "sol", "{input}");
    let rows = fx.runtime.task_experiments(&task).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].arm, None, "{rows:?}");
    assert_eq!(rows[0].exclusion_reason.as_deref(), Some("explicit_crew"));
    let readout = fx.runtime.experiment_readout(&experiment, None).unwrap();
    assert_eq!(readout.enrollment.explicit_crew_excluded, 1);
    assert_eq!(readout.enrollment.surface_conflict_excluded, 0);
    assert_eq!(crew_of(&fx, &task).0.as_deref(), Some("sol"));

    fx.runtime
        .force_experiment_assignment(&experiment, &task, "cheap-pool")
        .unwrap();
    let input = submit(&fx, &job, json!({ "task_id": task }));
    assert_eq!(input["crew_selection"]["crew"], "luna", "{input}");
    assert_eq!(
        crew_of(&fx, &task),
        (
            Some("luna".to_string()),
            Some(format!("experiment:{experiment}/cheap-pool"))
        )
    );
}
