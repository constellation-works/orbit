//! [ORB-19416] `task.crew` names the crew that runs: admission writes back a
//! crew an experiment arm or a provider hold chose, and a task with an
//! explicit crew stays out of crew experiments unless an operator forces it.
//!
//! In ws_orbit the cheap-pool arm of exp-4e3e0399 ran luna, haiku and
//! muse-spark while every enrolled task kept its creation-time crew, ORB-19405
//! ran grok under a codex capacity hold while still naming sol, and the
//! explicitly crewed ORB-19386 ran luna in the control arm.

use orbit_core::OrbitRuntime;
use orbit_core::TaskComplexity;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskUpdateParams;
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
                primary_metric: Some("total_tokens".into()),
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
    delivery_job_in_state(fx, "enabled")
}

/// [`delivery_job`] whose spec declares `state`.
fn delivery_job_in_state(fx: &Fixture, state: &str) -> std::path::PathBuf {
    let job_path = fx.repo.join("task_local_pipeline.yaml");
    std::fs::write(
        &job_path,
        format!(
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_local_pipeline\nspec:\n  state: {state}\n  steps: []\n"
        ),
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

/// Workspace config after a pool change: the medium pool now runs luna.
const CHANGED_POOL_CREWS: &str = r#"[workflow]
default_crew = "sol"
medium_complexity_crews = ["luna"]

[crews.sol]
provider = "codex"
model = "sol-model"

[crews.luna]
provider = "codex"
model = "luna-model"

[experiments]
enabled = true
"#;

fn enroll_both_arms(fx: &Fixture, job: &std::path::Path) -> (String, String) {
    enroll_both_arms_tagged(fx, job, &[])
}

fn enroll_both_arms_tagged(fx: &Fixture, job: &std::path::Path, tags: &[&str]) -> (String, String) {
    let (mut treated, mut control) = (None, None);
    for _ in 0..40 {
        let task = fx.tagged_task(tags, None).id;
        submit(fx, job, json!({ "task_id": task }));
        let arm = fx.runtime.task_experiments(&task).unwrap()[0].arm.clone();
        match arm.as_deref() {
            Some("cheap-pool") if treated.is_none() => treated = Some(task),
            Some("control") if control.is_none() => control = Some(task),
            _ => {}
        }
        if treated.is_some() && control.is_some() {
            break;
        }
    }
    (treated.unwrap(), control.unwrap())
}

/// Medium runs sol and hard runs luna, so a medium-to-hard rerate would
/// flag sol under today's tier pool.
const RERATE_CREWS: &str = r#"[workflow]
default_crew = "sol"
medium_complexity_crews = ["sol"]
hard_complexity_crews = ["luna"]

[crews.sol]
provider = "codex"
model = "sol-model"

[crews.luna]
provider = "codex"
model = "luna-model"

[experiments]
enabled = true
"#;

/// [`RERATE_CREWS`] plus an opus crew an operator can assign explicitly.
const RERATE_CREWS_WITH_OPERATOR_CREW: &str = r#"[workflow]
default_crew = "sol"
medium_complexity_crews = ["sol"]
hard_complexity_crews = ["luna"]

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

fn readout_with_config(
    fx: &Fixture,
    config: &str,
    experiment: &str,
) -> orbit_core::application::experiment::ExperimentReadout {
    std::fs::write(fx.repo.join(".orbit/config.toml"), config).unwrap();
    let global = fx.repo.parent().unwrap().join("global");
    let runtime = OrbitRuntime::from_roots(&global, &fx.repo.join(".orbit")).unwrap();
    runtime.experiment_readout(experiment, None).unwrap()
}

fn mismatches_for(
    readout: &orbit_core::application::experiment::ExperimentReadout,
    task: &str,
) -> Vec<String> {
    readout
        .warnings
        .iter()
        .filter(|warning| warning.kind == "crew_mismatch" && warning.message.contains(task))
        .map(|warning| warning.message.clone())
        .collect()
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

fn crew_assigned_entries(fx: &Fixture, task: &str) -> usize {
    fx.runtime
        .get_task_history(task)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == "crew_assigned")
        .count()
}

/// [ORB-19485] A refused submission leaves the task as it was. A disabled
/// delivery job is refused before any run is admitted, so the explicit crew
/// it names must not reach the task's crew, provenance or history.
#[test]
fn a_disabled_delivery_leaves_the_task_crew_and_history_unchanged() {
    if !isolated(
        "admitted_crew_write_back::a_disabled_delivery_leaves_the_task_crew_and_history_unchanged",
    ) {
        return;
    }
    let fx = fixture(HOLD_CREWS);
    let job = delivery_job_in_state(&fx, "disabled");
    let task = fx.tagged_task(&[], Some("sol")).id;
    let crew_before = crew_of(&fx, &task);
    let assigned_before = crew_assigned_entries(&fx, &task);

    let error = fx
        .runtime
        .submit_job_run(
            job.to_str().unwrap(),
            json!({ "task_id": task, "crew": "luna" }),
            Some("admitted-crew-test"),
        )
        .unwrap_err();
    assert!(error.to_string().contains("is disabled"), "{error}");

    assert_eq!(crew_of(&fx, &task), crew_before);
    assert_eq!(crew_assigned_entries(&fx, &task), assigned_before);
    // A disabled job has no catalog entry to filter runs by, so list them all:
    // the fixture workspace admitted nothing.
    let runs = fx
        .runtime
        .list_job_runs(JobRunListParams::default())
        .unwrap();
    assert!(runs.is_empty(), "{runs:?}");
}

/// [ORB-19485] An admitted delivery records its chosen crew and provenance
/// once: a later admission that draws the crew already recorded adds no entry.
#[test]
fn an_admitted_delivery_records_its_crew_exactly_once() {
    if !isolated("admitted_crew_write_back::an_admitted_delivery_records_its_crew_exactly_once") {
        return;
    }
    let fx = fixture(HOLD_CREWS);
    let job = delivery_job(&fx);
    let task = fx.tagged_task(&[], Some("sol")).id;
    let assigned_before = crew_assigned_entries(&fx, &task);

    submit(&fx, &job, json!({ "task_id": task, "crew": "luna" }));
    assert_eq!(
        crew_of(&fx, &task),
        (
            Some("luna".to_string()),
            Some("redraw:run_crew".to_string())
        )
    );
    assert_eq!(
        last_crew_note(&fx, &task),
        "crew changed from `sol` to `luna` at admission (run_crew)"
    );
    assert_eq!(crew_assigned_entries(&fx, &task), assigned_before + 1);

    submit(&fx, &job, json!({ "task_id": task, "crew": "luna" }));
    assert_eq!(crew_assigned_entries(&fx, &task), assigned_before + 1);
}

/// [ORB-19487] Changing the workspace pool after admission cannot invent a
/// control mismatch for the crew that was legitimate at admission.
#[test]
fn a_pool_change_after_admission_does_not_invent_a_control_mismatch() {
    if !isolated(
        "admitted_crew_write_back::a_pool_change_after_admission_does_not_invent_a_control_mismatch",
    ) {
        return;
    }
    let fx = fixture(EXPERIMENT_CREWS);
    let job = delivery_job(&fx);
    let experiment = started_crew_experiment(&fx.runtime);
    let (_treated, control) = enroll_both_arms(&fx, &job);
    assert_eq!(crew_of(&fx, &control).0.as_deref(), Some("sol"));

    let readout = readout_with_config(&fx, CHANGED_POOL_CREWS, &experiment);
    assert!(
        mismatches_for(&readout, &control).is_empty(),
        "{:?}",
        readout.warnings
    );
}

/// [ORB-19487] Changing the workspace pool after admission cannot erase a
/// genuine control mismatch either.
#[test]
fn a_pool_change_after_admission_does_not_erase_a_control_mismatch() {
    if !isolated(
        "admitted_crew_write_back::a_pool_change_after_admission_does_not_erase_a_control_mismatch",
    ) {
        return;
    }
    let fx = fixture(EXPERIMENT_CREWS);
    let job = delivery_job(&fx);
    let experiment = started_crew_experiment(&fx.runtime);
    let (_treated, control) = enroll_both_arms(&fx, &job);
    submit(&fx, &job, json!({ "task_id": control, "crew": "luna" }));
    assert_eq!(crew_of(&fx, &control).0.as_deref(), Some("luna"));

    let readout = readout_with_config(&fx, CHANGED_POOL_CREWS, &experiment);
    let mismatches = mismatches_for(&readout, &control);
    assert_eq!(mismatches.len(), 1, "{:?}", readout.warnings);
    assert!(
        mismatches[0].contains(&format!("task {control} in arm `control` ran crew `luna`")),
        "{mismatches:?}"
    );
}

/// [ORB-19487] A complexity rerate after admission cannot invent a
/// control mismatch: the admission tier's pool still applies.
#[test]
fn a_complexity_rerate_after_admission_does_not_invent_a_control_mismatch() {
    if !isolated(
        "admitted_crew_write_back::a_complexity_rerate_after_admission_does_not_invent_a_control_mismatch",
    ) {
        return;
    }
    let fx = fixture(RERATE_CREWS);
    let job = delivery_job(&fx);
    let experiment = started_crew_experiment(&fx.runtime);
    let (_treated, control) = enroll_both_arms(&fx, &job);
    assert_eq!(crew_of(&fx, &control).0.as_deref(), Some("sol"));
    fx.runtime
        .update_task_with_identity(
            &control,
            TaskUpdateParams {
                complexity: Some(TaskComplexity::Hard),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    // ORB-19929: the tier changes, while the admitted crew remains the
    // readout's actual implementation identity.
    assert_eq!(crew_of(&fx, &control).0.as_deref(), Some("sol"));

    let readout = fx.runtime.experiment_readout(&experiment, None).unwrap();
    assert!(
        mismatches_for(&readout, &control).is_empty(),
        "{:?}",
        readout.warnings
    );
}

/// [ORB-19487] A run-level pool override is the admission pool for that
/// delivery, not today's workspace pool.
#[test]
fn a_run_level_pool_override_is_checked_against_its_admission_pool() {
    if !isolated(
        "admitted_crew_write_back::a_run_level_pool_override_is_checked_against_its_admission_pool",
    ) {
        return;
    }
    let fx = fixture(EXPERIMENT_CREWS);
    let job = delivery_job(&fx);
    let experiment = started_crew_experiment(&fx.runtime);
    let (_treated, control) = enroll_both_arms(&fx, &job);
    let input = submit(
        &fx,
        &job,
        json!({ "task_id": control, "medium_complexity_crews": ["luna"] }),
    );
    assert_eq!(input["crew_selection"]["crew"], "luna", "{input}");
    assert_eq!(crew_of(&fx, &control).0.as_deref(), Some("luna"));

    let readout = fx.runtime.experiment_readout(&experiment, None).unwrap();
    assert!(
        mismatches_for(&readout, &control).is_empty(),
        "{:?}",
        readout.warnings
    );
}

/// [ORB-19487] A valid purpose route is checked against its recorded
/// admission pool, not the complexity pool.
#[test]
fn a_purpose_route_is_checked_against_its_admission_pool() {
    if !isolated("admitted_crew_write_back::a_purpose_route_is_checked_against_its_admission_pool")
    {
        return;
    }
    const PURPOSE_CREWS: &str = r#"[workflow]
default_crew = "sol"
medium_complexity_crews = ["sol"]

[workflow.purpose_crews]
baseline_red_repair = ["luna"]

[crews.sol]
provider = "codex"
model = "sol-model"

[crews.luna]
provider = "codex"
model = "luna-model"

[experiments]
enabled = true
"#;
    let fx = fixture(PURPOSE_CREWS);
    let job = delivery_job(&fx);
    let experiment = started_crew_experiment(&fx.runtime);
    let (_treated, control) =
        enroll_both_arms_tagged(&fx, &job, &["crew-purpose:baseline_red_repair"]);
    assert_eq!(crew_of(&fx, &control).0.as_deref(), Some("luna"));

    let readout = fx.runtime.experiment_readout(&experiment, None).unwrap();
    assert!(
        mismatches_for(&readout, &control).is_empty(),
        "{:?}",
        readout.warnings
    );
}

/// [ORB-19487] Without recorded admission evidence a control crew is
/// reported unavailable, not as a confirmed mismatch.
#[test]
fn a_control_crew_without_admission_evidence_is_unavailable_not_a_mismatch() {
    if !isolated(
        "admitted_crew_write_back::a_control_crew_without_admission_evidence_is_unavailable_not_a_mismatch",
    ) {
        return;
    }
    let fx = fixture(EXPERIMENT_CREWS);
    let job = delivery_job(&fx);
    let experiment = started_crew_experiment(&fx.runtime);
    let (_treated, control) = enroll_both_arms(&fx, &job);
    // Simulate a pre-capture delivery: remove every job run for the task.
    {
        use orbit_store::contracts::JobRunQuery;
        let jobs = orbit_store::compose::workspace_job_run_store(
            fx.runtime.sqlite_store().unwrap(),
            fx.runtime.workspace_id().unwrap(),
        );
        for run in jobs
            .list_job_runs_filtered(&JobRunQuery {
                task_id: Some(control.clone()),
                include_steps: false,
                ..Default::default()
            })
            .unwrap()
        {
            jobs.delete_job_run(&run.run_id.to_string()).unwrap();
        }
    }

    let readout = fx.runtime.experiment_readout(&experiment, None).unwrap();
    assert!(
        mismatches_for(&readout, &control).is_empty(),
        "{:?}",
        readout.warnings
    );
    assert!(
        readout.warnings.iter().any(|warning| {
            warning.message.contains(&control)
                && (warning.kind.contains("unavailable")
                    || warning.message.to_lowercase().contains("unavailable"))
        }),
        "{:?}",
        readout.warnings
    );
}

/// [ORB-19602] The backfill leaves a task alone when its explicit crew
/// assignment is newer than the delivery run it would copy from, so an
/// operator's choice stands over a stale selection.
#[test]
fn backfill_leaves_an_explicit_crew_newer_than_its_latest_run() {
    if !isolated(
        "admitted_crew_write_back::backfill_leaves_an_explicit_crew_newer_than_its_latest_run",
    ) {
        return;
    }
    let fx = fixture(HOLD_CREWS);
    let job = delivery_job(&fx);
    let task = fx.tagged_task(&[], None).id;
    let input = submit(&fx, &job, json!({ "task_id": task, "crew": "luna" }));
    assert_eq!(input["crew_selection"]["crew"], "luna", "{input}");
    assert_eq!(crew_of(&fx, &task).0.as_deref(), Some("luna"));
    fx.runtime
        .update_task_with_identity(
            &task,
            TaskUpdateParams {
                crew: Some(Some("opus".to_string())),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        crew_of(&fx, &task),
        (Some("opus".to_string()), Some("explicit".to_string()))
    );

    let report = fx.runtime.backfill_admitted_task_crews().unwrap();
    assert!(
        report.tasks.iter().all(|change| change.task_id != task),
        "{report:?}"
    );
    assert_eq!(
        crew_of(&fx, &task),
        (Some("opus".to_string()), Some("explicit".to_string()))
    );
}

/// [ORB-19602] The backfill skips a latest run cancelled before it
/// implemented: its crew selection never ran, so it must not replace the
/// task's crew. The seeded legacy redraw below is a system assignment, not
/// an explicit one, so only the cancellation can spare the task.
#[test]
fn backfill_skips_a_latest_run_cancelled_before_implementation() {
    if !isolated(
        "admitted_crew_write_back::backfill_skips_a_latest_run_cancelled_before_implementation",
    ) {
        return;
    }
    let fx = fixture(RERATE_CREWS);
    let job = delivery_job(&fx);
    let task = fx.tagged_task(&[], None).id;
    assert_eq!(crew_of(&fx, &task).0.as_deref(), Some("sol"));
    let submitted = fx
        .runtime
        .submit_job_run(
            job.to_str().unwrap(),
            json!({ "task_id": task }),
            Some("admitted-crew-test"),
        )
        .unwrap();
    fx.runtime.cancel_job_run(&submitted.run_id).unwrap();
    // Seed the historical system redraw that older binaries allowed after
    // cancellation, without an explicit assignment.
    seed_historical_scope_redraw(&fx, &task);
    assert_eq!(crew_of(&fx, &task).0.as_deref(), Some("luna"));

    let report = fx.runtime.backfill_admitted_task_crews().unwrap();
    assert!(
        report.tasks.iter().all(|change| change.task_id != task),
        "{report:?}"
    );
    assert_eq!(crew_of(&fx, &task).0.as_deref(), Some("luna"));
}

/// [ORB-19623] An explicit crew assignment that lands after the backfill has
/// read a delivery but before it writes the task's crew is newer than that
/// delivery, so the operator's choice stands. The checkpoint lands it in that
/// window, which a thread race would only hit sometimes.
#[test]
fn backfill_does_not_overwrite_an_explicit_crew_assigned_during_the_backfill() {
    if !isolated(
        "admitted_crew_write_back::backfill_does_not_overwrite_an_explicit_crew_assigned_during_the_backfill",
    ) {
        return;
    }
    let fx = fixture(RERATE_CREWS_WITH_OPERATOR_CREW);
    let job = delivery_job(&fx);
    let task = fx.tagged_task(&[], None).id;
    let input = submit(&fx, &job, json!({ "task_id": task }));
    assert_eq!(input["crew_selection"]["crew"], "sol", "{input}");
    // Seed an older binary's scope redraw: the task no longer names the sol
    // its delivery ran, so the backfill has a historical crew to repair.
    seed_historical_scope_redraw(&fx, &task);
    assert_eq!(crew_of(&fx, &task).0.as_deref(), Some("luna"));

    let ran = std::rc::Rc::new(std::cell::Cell::new(false));
    let hook = {
        let owner = fx.runtime.clone();
        let task = task.clone();
        let ran = ran.clone();
        move || {
            owner
                .update_task_with_identity(
                    &task,
                    TaskUpdateParams {
                        crew: Some(Some("opus".to_string())),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap();
            ran.set(true);
        }
    };
    let report = orbit_core::test_support::with_crew_backfill_checkpoint(hook, || {
        fx.runtime.backfill_admitted_task_crews()
    })
    .unwrap();
    assert!(ran.get(), "the checkpoint never landed the explicit crew");
    assert!(
        report.tasks.iter().all(|change| change.task_id != task),
        "{report:?}"
    );
    assert_eq!(
        crew_of(&fx, &task),
        (Some("opus".to_string()), Some("explicit".to_string()))
    );
}

/// [ORB-19520] A rerate onto a superseded delivery's crew is unavailable,
/// not a mismatch: the latest relevant delivery ran a different crew.
#[test]
fn a_rerate_onto_a_superseded_delivery_crew_is_unavailable_not_a_mismatch() {
    if !isolated(
        "admitted_crew_write_back::a_rerate_onto_a_superseded_delivery_crew_is_unavailable_not_a_mismatch",
    ) {
        return;
    }
    let fx = fixture(RERATE_CREWS);
    let job = delivery_job(&fx);
    let experiment = started_crew_experiment(&fx.runtime);
    let (_treated, target) = enroll_both_arms(&fx, &job);
    assert_eq!(crew_of(&fx, &target).0.as_deref(), Some("sol"));
    // Drop the enrolling delivery so the history below holds exactly the
    // older explicit luna delivery and the newer normal sol delivery.
    {
        use orbit_store::contracts::JobRunQuery;
        let jobs = orbit_store::compose::workspace_job_run_store(
            fx.runtime.sqlite_store().unwrap(),
            fx.runtime.workspace_id().unwrap(),
        );
        for run in jobs
            .list_job_runs_filtered(&JobRunQuery {
                task_id: Some(target.clone()),
                include_steps: false,
                ..Default::default()
            })
            .unwrap()
        {
            jobs.delete_job_run(&run.run_id.to_string()).unwrap();
        }
    }

    // Older delivery: an explicit luna run against the medium pool.
    let explicit = submit(&fx, &job, json!({ "task_id": target, "crew": "luna" }));
    assert_eq!(explicit["crew_selection"]["crew"], "luna", "{explicit}");
    assert_eq!(crew_of(&fx, &target).0.as_deref(), Some("luna"));

    // Re-queue onto the medium pool without another delivery, so the next
    // admission runs the pool's sol without changing the pool assignment.
    fx.runtime
        .update_task_with_identity(
            &target,
            TaskUpdateParams {
                crew: Some(None),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(crew_of(&fx, &target).0.as_deref(), Some("sol"));

    // Newer delivery: a normal admission that runs the medium pool's sol.
    let admitted = submit(&fx, &job, json!({ "task_id": target }));
    assert_eq!(admitted["crew_selection"]["crew"], "sol", "{admitted}");
    assert_eq!(crew_of(&fx, &target).0.as_deref(), Some("sol"));

    // Seed an older binary's medium-to-hard redraw without another delivery.
    // Current scope updates preserve attribution, but readout must still
    // handle these historical records without inventing a mismatch.
    seed_historical_scope_redraw(&fx, &target);
    assert_eq!(crew_of(&fx, &target).0.as_deref(), Some("luna"));

    let readout = fx.runtime.experiment_readout(&experiment, None).unwrap();
    assert!(
        mismatches_for(&readout, &target).is_empty(),
        "{:?}",
        readout.warnings
    );
    assert!(
        readout.warnings.iter().any(|warning| {
            warning.message.contains(&target)
                && (warning.kind.contains("unavailable")
                    || warning.message.to_lowercase().contains("unavailable"))
        }),
        "{:?}",
        readout.warnings
    );
}

/// Seed the persisted ORB-19929 defect for historical repair/readout checks.
/// New public scope updates no longer create this inconsistent attribution.
fn seed_historical_scope_redraw(fx: &Fixture, task: &str) {
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&fx.runtime.global_root()),
    )
    .unwrap();
    let backends = orbit_store::compose::workspace_coordinated_backends(
        registry,
        fx.runtime.workspace_id().unwrap(),
        fx.runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    backends
        .document
        .update_task_document(
            task,
            orbit_store::contracts::TaskDocumentUpdateParams {
                actor: "historical-scope-redraw-fixture".into(),
                crew: Some(Some("luna".into())),
                crew_source: Some(Some("pool:hard".into())),
                complexity: Some(TaskComplexity::Hard),
                ..Default::default()
            },
        )
        .unwrap();
}
