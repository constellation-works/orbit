//! [ORB-15202] What a leaf's unsuccessful outcome means to the gate and auto
//! parents that wait on it.
//!
//! A required check red on the base and a before-landing review refusal that
//! awaits a recorded decision end the run `held`, not `failed`. A cancelled
//! child ends a parent that only waited on it `cancelled`. A parent that
//! fails because a child did records that leaf as a typed `root_cause`, so a
//! failed-run listing folds the chain into one incident.
//!
//! Each run executes one `pipeline_success_guard` step through the detached
//! worker, the step a gate's `require_child_success` and an auto pipeline's
//! `require_gate_success` run, over a child result named in its input.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use orbit_core::application::job::{fold_run_incidents, job_run_to_json};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::workflow::{
    BASELINE_RED_HOLD_EVENT, BaselineRedHold, JobRun, JobRunState, PipelineState,
    REVIEW_LANDING_DECISION_PENDING, RunRootCause,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

const JOB: &str = "cascade_fixture";

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    jobs: Arc<dyn JobRunStoreBackend>,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            repo.join(".orbit/config.toml"),
            "[workflow]\ndefault_crew = \"fixture\"\n\n\
             [crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
        )
        .unwrap();
        std::fs::write(
            jobs_dir.join(format!("{JOB}.yaml")),
            serde_json::to_string(&json!({
                "schemaVersion": 2, "kind": "Job",
                "metadata": {"name": JOB},
                "spec": {"state": "enabled", "steps": [{
                    "id": "require_child_success",
                    "spec": {
                        "type": "deterministic",
                        "action": "pipeline_success_guard",
                        "config": {},
                    },
                    "default_input": {
                        "context": "fixture child run",
                        "results": "{{ input.results }}",
                    },
                }]}
            }))
            .unwrap(),
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            runtime,
            repo,
            jobs,
        }
    }

    /// A queued run of the fixture job over `input`.
    fn queued(&self, input: Value) -> String {
        let run = self
            .jobs
            .insert_job_run(JOB, 1, Utc::now(), Some(input.clone()), None)
            .unwrap();
        self.jobs
            .write_run_state(
                &run.run_id,
                &PipelineState::new(run.run_id.clone(), run.job_id, input),
            )
            .unwrap();
        run.run_id
    }

    /// Run a queued run's worker to its terminal state.
    fn execute(&self, run_id: &str) -> JobRun {
        // The worker reports an unsuccessful run as an error; the stored run
        // is what callers read.
        let _ = self.runtime.execute_pipeline_run_worker(run_id);
        self.stored(run_id)
    }

    /// Queue and execute a run whose guard checks `results`.
    fn guard(&self, task: Option<&str>, results: Value) -> JobRun {
        let mut input = json!({ "results": results });
        if let Some(task) = task {
            input["task_ids"] = json!([task]);
        }
        let run_id = self.queued(input);
        self.execute(&run_id)
    }

    fn stored(&self, run_id: &str) -> JobRun {
        self.jobs.get_job_run(run_id).unwrap().unwrap()
    }

    fn state(&self, run_id: &str) -> PipelineState {
        self.jobs.read_run_state(run_id).unwrap().unwrap()
    }

    /// An admissible backlog task.
    fn task(&self) -> String {
        std::fs::write(self.repo.join("feature.txt"), "feature\n").unwrap();
        self.runtime
            .add_task(TaskAddParams {
                title: "Deliver the feature".to_string(),
                description: "Make the feature work.".to_string(),
                context_files: vec!["file:feature.txt".to_string()],
                complexity: TaskComplexity::Medium,
                task_type: Some(TaskType::Bug),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    /// Move `task` in progress under `run_id`, as admission does.
    fn couple(&self, task: &str, run_id: &str) {
        self.runtime
            .apply_task_automation_update(
                task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::InProgress),
                    job_run_id: Some(run_id.to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
    }
}

/// The error codes a run's recorded steps carry.
fn step_codes(run: &JobRun) -> Vec<String> {
    run.steps
        .iter()
        .filter_map(|step| step.error_code.clone())
        .collect()
}

/// The error text a run's last failed step records.
fn failure_text(run: &JobRun) -> String {
    run.steps
        .iter()
        .rev()
        .find_map(|step| step.error_message.clone())
        .unwrap_or_default()
}

fn baseline_red_text() -> String {
    BaselineRedHold {
        base_ref: "origin/agent-main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        command: "make ci-fast".to_string(),
        run_id: "jrun-observed".to_string(),
        selection: None,
    }
    .text("the required check fails on the base exactly as on the candidate")
}

#[test]
fn a_red_base_and_a_pending_landing_decision_end_held_and_other_failures_fail() {
    if !isolated(
        "cascade_outcomes::a_red_base_and_a_pending_landing_decision_end_held_and_other_failures_fail",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let pending = format!(
        "before-landing review did not approve; {REVIEW_LANDING_DECISION_PENDING} (`orbit review \
         gate decide`)"
    );
    for (error, expected, code) in [
        (baseline_red_text(), JobRunState::Held, Some("baseline_red")),
        (pending, JobRunState::Held, Some("review_decision_pending")),
        (
            "cargo test failed: 2 tests failed".to_string(),
            JobRunState::Failed,
            None,
        ),
    ] {
        let run = fixture.guard(None, json!([{ "status": "failed", "error": error }]));
        assert_eq!(run.state, expected, "{error}: {:?}", run.steps);
        if let Some(code) = code {
            assert!(
                step_codes(&run).iter().any(|recorded| recorded == code),
                "{error}: {:?}",
                run.steps
            );
        }
    }
}

#[test]
fn a_leaf_held_on_a_red_base_still_holds_its_task_in_the_backlog() {
    if !isolated("cascade_outcomes::a_leaf_held_on_a_red_base_still_holds_its_task_in_the_backlog")
    {
        return;
    }
    let fixture = Fixture::new();
    let task = fixture.task();
    let run_id = fixture.queued(json!({
        "task_ids": [task],
        "results": [{ "status": "failed", "error": baseline_red_text() }],
    }));
    fixture.couple(&task, &run_id);

    let run = fixture.execute(&run_id);

    assert_eq!(run.state, JobRunState::Held, "{:?}", run.steps);
    assert_eq!(
        fixture.runtime.get_task(&task).unwrap().status,
        TaskStatus::Backlog
    );
    let history = fixture.runtime.get_task_history(&task).unwrap();
    assert!(
        history
            .iter()
            .any(|entry| entry.event == BASELINE_RED_HOLD_EVENT),
        "{history:?}"
    );
}

#[test]
fn a_cancelled_leaf_cancels_the_gate_and_auto_parents_that_wait_on_it() {
    if !isolated(
        "cascade_outcomes::a_cancelled_leaf_cancels_the_gate_and_auto_parents_that_wait_on_it",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let leaf = fixture.queued(json!({}));
    fixture
        .jobs
        .finalize_job_run(&leaf, JobRunState::Cancelled, Utc::now(), None)
        .unwrap();

    let gate = fixture.guard(None, json!([{ "status": "cancelled", "run_id": leaf }]));
    assert_eq!(gate.state, JobRunState::Cancelled, "{:?}", gate.steps);
    assert!(
        step_codes(&gate)
            .iter()
            .any(|code| code == "child_cancelled"),
        "{:?}",
        gate.steps
    );
    assert_eq!(fixture.state(&gate.run_id).root_cause, None);

    let auto = fixture.guard(
        None,
        json!([{ "status": "cancelled", "run_id": gate.run_id }]),
    );
    assert_eq!(auto.state, JobRunState::Cancelled, "{:?}", auto.steps);

    // A failure beside the cancellation is still a failure.
    let mixed = fixture.guard(
        None,
        json!([
            { "status": "cancelled", "run_id": leaf },
            { "status": "failed", "error": "cargo test failed" },
        ]),
    );
    assert_eq!(mixed.state, JobRunState::Failed, "{:?}", mixed.steps);
}

#[test]
fn cascaded_parents_name_their_leaf_and_fold_into_one_incident() {
    if !isolated("cascade_outcomes::cascaded_parents_name_their_leaf_and_fold_into_one_incident") {
        return;
    }
    let fixture = Fixture::new();
    let task = "ORB-90001";
    let leaf = fixture.guard(
        Some(task),
        json!([{ "status": "failed", "error": "[validation_failed] cargo test failed" }]),
    );
    assert_eq!(leaf.state, JobRunState::Failed, "{:?}", leaf.steps);
    assert_eq!(fixture.state(&leaf.run_id).root_cause, None);
    let expected = RunRootCause {
        leaf_run_id: leaf.run_id.clone(),
        task_id: Some(task.to_string()),
        step: Some("require_child_success".to_string()),
        code: Some("validation_failed".to_string()),
    };

    let gate = fixture.guard(
        Some(task),
        json!([{ "status": "failed", "run_id": leaf.run_id, "error": failure_text(&leaf) }]),
    );
    assert_eq!(gate.state, JobRunState::Failed, "{:?}", gate.steps);
    assert_eq!(
        fixture.state(&gate.run_id).root_cause.as_ref(),
        Some(&expected)
    );

    let auto = fixture.guard(
        None,
        json!([{ "status": "failed", "run_id": gate.run_id, "error": failure_text(&gate) }]),
    );
    assert_eq!(auto.state, JobRunState::Failed, "{:?}", auto.steps);
    assert_eq!(
        fixture.state(&auto.run_id).root_cause.as_ref(),
        Some(&expected),
        "an auto parent inherits its gate's leaf"
    );
    let text = failure_text(&auto);
    assert!(text.contains(&leaf.run_id), "{text}");
    assert!(
        !text.contains("[validation_failed]"),
        "the auto parent names the leaf instead of nesting its text: {text}"
    );
    let projected = job_run_to_json(&auto, Some(&fixture.state(&auto.run_id)));
    assert_eq!(projected["root_cause"]["leaf_run_id"], leaf.run_id.as_str());
    assert_eq!(projected["root_cause"]["task_id"], task);

    let runs = [auto.clone(), gate.clone(), leaf.clone()];
    let states = runs
        .iter()
        .map(|run| fixture.state(&run.run_id))
        .collect::<Vec<_>>();
    let incidents = fold_run_incidents(&runs, &states.iter().map(Some).collect::<Vec<_>>());
    assert_eq!(incidents.len(), 1, "{incidents:?}");
    assert_eq!(incidents[0].index, 2, "the leaf represents its incident");
    assert_eq!(incidents[0].leaf_run_id, leaf.run_id);
    assert_eq!(
        incidents[0].cascaded_run_ids,
        vec![auto.run_id.clone(), gate.run_id.clone()]
    );
}
