//! Fresh admission authority is separate from immutable enrollment.
use super::*;
use orbit_types::workflow::JobRunState;

struct Fixture {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    job: std::path::PathBuf,
    experiment: String,
    task: String,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("config.toml"),
            r#"
[workflow]
default_crew = "normal"
medium_complexity_crews = ["normal"]
[crews.normal]
provider = "codex"
model = "test"
[crews.treated]
provider = "codex"
model = "test"
[experiments]
enabled = true
"#,
        )
        .unwrap();
        let job = capture_job(root.path());
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let experiment = runtime.create_experiment("admission-authority", ExperimentDefinitionInput {
            mode: Some("live".into()),
            arms: Some(json!([
                {"name":"control", "control":true, "weight":1, "overlay":{"crew":{"pool":[{"crew":"treated","weight":1}]}}},
                {"name":"treatment", "weight":1, "overlay":{"crew":{"pool":[{"crew":"treated","weight":1}]}}}
            ])),
            primary_metric: Some("total_tokens".into()),
            ..Default::default()
        }, actor()).unwrap().id;
        runtime
            .change_experiment(&experiment, ExperimentChange::Start, actor())
            .unwrap();
        let task = runtime
            .add_task(TaskAddParams {
                title: "Fresh admission after experiment or operator changes".into(),
                complexity: TaskComplexity::Medium,
                task_type: Some(TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id;
        Self {
            _root: root,
            runtime,
            job,
            experiment,
            task,
        }
    }
    fn admit(&self, crew: Option<&str>) -> orbit_types::workflow::JobRun {
        let mut input = json!({"task_id":self.task});
        if let Some(crew) = crew {
            input["crew"] = json!(crew);
        }
        let submitted = self
            .runtime
            .submit_job_run(self.job.to_str().unwrap(), input, Some("fixture"))
            .unwrap();
        self.runtime
            .wait_pipeline_runs(
                std::slice::from_ref(&submitted.run_id),
                10,
                1,
                Some("fixture"),
            )
            .unwrap();
        self.runtime
            .show_job_run_observed(&submitted.run_id)
            .unwrap()
    }
    fn pin(&self) {
        self.runtime
            .update_task_as_human(
                &self.task,
                TaskUpdateParams {
                    crew: Some(Some("normal".into())),
                    ..Default::default()
                },
                "fixture operator".into(),
            )
            .unwrap();
    }
}
fn actor() -> ExperimentActor {
    ExperimentActor {
        actor: "fixture operator".into(),
        reason: Some("admission regression".into()),
    }
}

#[test]
fn ended_treatments_do_not_authorize_fresh_admission() {
    if !super::super::dispatch_admission::isolated(
        "experiment_lifecycle::admission::ended_treatments_do_not_authorize_fresh_admission",
    ) {
        return;
    }
    for end in [
        ExperimentChange::Conclude { arm: None },
        ExperimentChange::Abandon,
    ] {
        let f = Fixture::new();
        let first = f.admit(None);
        let frozen = first.input.as_ref().unwrap();
        assert_eq!(frozen["crew_selection"]["crew"], "treated");
        let rows = f.runtime.task_experiments(&f.task).unwrap();
        f.runtime
            .change_experiment(&f.experiment, end, actor())
            .unwrap();
        let events = serde_json::to_value(f.runtime.experiment_view(&f.experiment).unwrap())
            .unwrap()["events"]
            .clone();
        let next = f.admit(None);
        let input = next.input.unwrap();
        assert_eq!(
            input["crew_selection"]["crew"], "normal",
            "an ended experiment cannot authorize fresh treatment"
        );
        assert_eq!(input["experiment_capture"]["assignments"][0]["live"], false);
        assert!(input["crew_selection"]["experiment_overlay_digest"].is_null());
        assert_eq!(f.runtime.task_experiments(&f.task).unwrap(), rows);
        assert_eq!(
            serde_json::to_value(f.runtime.experiment_view(&f.experiment).unwrap()).unwrap()["events"],
            events
        );
        assert_eq!(
            f.runtime
                .show_job_run_observed(&first.run_id)
                .unwrap()
                .input
                .as_ref(),
            Some(frozen)
        );
        // Public resume must copy the admitted contract even after conclusion.
        let mut failed = first.clone();
        failed.state = JobRunState::Failed;
        failed.finished_at = Some(chrono::Utc::now());
        f.runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &failed, None)
            .unwrap();
        let resumed = f
            .runtime
            .submit_resume_run(&first.run_id, Some("fixture"), None)
            .unwrap();
        f.runtime
            .wait_pipeline_runs(
                std::slice::from_ref(&resumed.run_id),
                10,
                1,
                Some("fixture"),
            )
            .unwrap();
        let resumed = f
            .runtime
            .show_job_run_observed(&resumed.run_id)
            .unwrap()
            .input
            .unwrap();
        assert_eq!(resumed["experiment_capture"], frozen["experiment_capture"]);
        assert_eq!(resumed["crew_selection"], frozen["crew_selection"]);
        f.pin();
        assert_eq!(
            f.admit(None).input.unwrap()["crew_selection"]["crew"],
            "normal"
        );
        assert_eq!(
            f.admit(Some("normal")).input.unwrap()["crew_selection"]["source"],
            "explicit"
        );
    }
}

#[test]
fn later_operator_crew_wins_over_running_enrollment_and_force() {
    if !super::super::dispatch_admission::isolated(
        "experiment_lifecycle::admission::later_operator_crew_wins_over_running_enrollment_and_force",
    ) {
        return;
    }
    let f = Fixture::new();
    f.admit(None);
    let rows = f.runtime.task_experiments(&f.task).unwrap();
    f.pin();
    let fresh = f.admit(None).input.unwrap();
    assert_eq!(fresh["crew_selection"]["crew"], "normal");
    assert_eq!(fresh["crew_selection"]["source"], "task.crew");
    assert_eq!(fresh["experiment_capture"]["assignments"][0]["live"], false);
    assert_eq!(f.runtime.task_experiments(&f.task).unwrap(), rows);
    assert!(fresh["crew_selection"]["experiment_suppressions"].is_array());
    let f = Fixture::new();
    f.pin();
    f.runtime
        .force_experiment_assignment(&f.experiment, &f.task, "treatment")
        .unwrap();
    assert_eq!(
        f.admit(None).input.unwrap()["crew_selection"]["crew"],
        "treated"
    );
    let explicit = f.admit(Some("normal")).input.unwrap();
    assert_eq!(explicit["crew_selection"]["source"], "explicit");
    assert!(explicit["crew_selection"]["experiment_overlay_digest"].is_null());
    f.pin();
    assert_eq!(
        f.admit(None).input.unwrap()["crew_selection"]["crew"],
        "normal"
    );
    assert!(f.runtime.task_experiments(&f.task).unwrap()[0].forced);
}

#[test]
fn explicit_same_crew_does_not_count_as_experiment_treatment() {
    if !super::super::dispatch_admission::isolated(
        "experiment_lifecycle::admission::explicit_same_crew_does_not_count_as_experiment_treatment",
    ) {
        return;
    }
    use orbit_store::contracts::ExperimentStoreBackend as _;
    let f = Fixture::new();
    // Fix the enrolled arm to isolate readout behavior from hash allocation.
    f.runtime
        .sqlite_store()
        .unwrap()
        .assign_experiments(
            &f.runtime.workspace_id().unwrap(),
            &f.task,
            true,
            true,
            &|records, _, _| {
                let record = records
                    .iter()
                    .find(|record| record.experiment.id == f.experiment)
                    .unwrap();
                let arm = record
                    .arms
                    .iter()
                    .find(|arm| arm.name == "treatment")
                    .unwrap();
                Ok(vec![orbit_store::contracts::ExperimentAssignment {
                    experiment: f.experiment.clone(),
                    task_id: f.task.clone(),
                    arm: Some(arm.name.clone()),
                    segment: Some(record.experiment.segment),
                    digest: Some(arm.overlay_digest.clone()),
                    assigned_at: chrono::Utc::now(),
                    forced: false,
                    exclusion_reason: None,
                }])
            },
        )
        .unwrap();
    f.admit(None);
    let before = f.runtime.experiment_readout(&f.experiment, None).unwrap();
    let treatment = before
        .arms
        .iter()
        .find(|arm| arm.name == "treatment")
        .unwrap();
    assert_eq!(treatment.delivery.as_ref().unwrap().delivered, 1);
    let explicit = f.admit(Some("treated")).input.unwrap();
    assert_eq!(explicit["crew_selection"]["source"], "explicit");
    assert!(explicit["crew_selection"]["experiment_overlay_digest"].is_null());
    let after = f.runtime.experiment_readout(&f.experiment, None).unwrap();
    let treatment = after
        .arms
        .iter()
        .find(|arm| arm.name == "treatment")
        .unwrap();
    assert_eq!(treatment.n, 1);
    assert_eq!(
        treatment.delivery.as_ref().unwrap().delivered,
        0,
        "an explicit choice is not delivered experimental treatment even when crew names coincide"
    );
    assert_eq!(
        treatment.assignment_digests,
        before
            .arms
            .iter()
            .find(|arm| arm.name == "treatment")
            .unwrap()
            .assignment_digests
    );
}
