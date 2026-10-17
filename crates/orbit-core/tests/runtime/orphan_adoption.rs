//! An owner drain pass adopts the open pull request of a delivery leaf whose
//! worker died after `pr_open`: it resumes the interrupted run once, through
//! checkpoint resume, so completion and auto-fix continue on the same pull
//! request under the run's retry lineage. A run is left alone when the drain
//! lacks the authority it was started with (until a drain holding it passes),
//! and for good when its pull request is no longer open, its task moved on or
//! was deleted, it never opened one, or it is itself a resumed attempt. One
//! run's store error, an unreachable forge (read once per pass) and an
//! unloadable activity catalog defer instead of deciding, and nothing is
//! adopted under host resource pressure.
//!
//! The pass runs as the drain's own `adopt_orphaned_deliveries` step through
//! the runtime's deterministic dispatch, against a fake forge. Resumed runs
//! start a substitute worker that waits for the fixture to end, so a retry
//! stays the lineage's live attempt while the test asserts.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, TaskPrForge};
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::{JobRunStepParams, JobRunStoreBackend};
use orbit_tools::ToolContext;
use orbit_types::task::ExternalRef;
use orbit_types::workflow::{JobRunState, JobTargetType, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

/// The forge's open pull requests; adoption only reads them, one at a time.
#[derive(Default)]
struct ForgeState {
    open: Vec<ForgePullRequest>,
    /// Every read answers with an error, as an unreachable GitHub does.
    unreachable: bool,
    reads: usize,
}

#[derive(Default, Clone)]
struct FakeForge(Arc<Mutex<ForgeState>>);

impl FakeForge {
    fn open(&self, number: u64, head_branch: &str) {
        self.0.lock().unwrap().open.push(ForgePullRequest {
            number,
            head_branch: head_branch.to_string(),
            body: String::new(),
            author: "orbit-bot".to_string(),
        });
    }
}

impl TaskPrForge for FakeForge {
    fn open_pull_requests(&self, _repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(self.0.lock().unwrap().open.clone())
    }

    fn open_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
    ) -> Result<Option<ForgePullRequest>, OrbitError> {
        let mut state = self.0.lock().unwrap();
        state.reads += 1;
        if state.unreachable {
            return Err(OrbitError::Execution("gh pr view failed: timed out".into()));
        }
        Ok(state
            .open
            .iter()
            .find(|pull| pull.number == number)
            .cloned())
    }

    fn authenticated_login(&self, _repo_root: &Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".to_string())
    }

    fn close_pull_request(&self, _: &Path, number: u64, _: &str) -> Result<(), OrbitError> {
        panic!("adoption never closes a pull request (#{number})")
    }
}

/// A delivery leaf that published through `pr_open` and then waits on its
/// pull request.
const LEAF_YAML: &str = "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_pr_pipeline\nspec:\n  state: enabled\n  task_delivery: {}\n  steps:\n    - {id: push, target: 'activity:git_push'}\n    - {id: pr_open, target: 'activity:pr_open'}\n    - {id: complete_pr, target: 'activity:pr_complete'}\n";

struct Fixture {
    _root: TempDir,
    activities: std::path::PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
    forge: FakeForge,
    probe: Arc<super::dispatch_admission::PressureProbe>,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        crate::worker_fixture::install(root.path(), "removed");
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        let activities = global.join("resources/activities");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(&activities).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(jobs_dir.join("task_pr_pipeline.yaml"), LEAF_YAML).unwrap();
        for name in ["git_push", "pr_open", "pr_complete"] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("assets/activities/{name}.yaml")),
                activities.join(format!("{name}.yaml")),
            )
            .unwrap();
        }
        let forge = FakeForge::default();
        let probe = super::dispatch_admission::PressureProbe::calm();
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_task_pr_forge(Arc::new(forge.clone()))
            .with_host_resource_probe(probe.clone());
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            activities,
            runtime,
            jobs,
            forge,
            probe,
        }
    }

    /// A task in `review` whose `--complete --auto-fix` delivery leaf opened
    /// pull request `number` (when given) and then lost its worker. Returns
    /// the task, the interrupted run and the pull request's head branch.
    fn dead_leaf(&self, number: Option<u64>) -> (String, String, String) {
        let task = self
            .runtime
            .add_task(TaskAddParams {
                title: "Orphaned delivery".into(),
                description: "A delivery whose leaf died after opening its PR.".into(),
                acceptance_criteria: vec!["Its PR lands.".into()],
                plan: "Deliver it.".into(),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id;
        let head = format!("orbit/{task}-leaf");
        let input = json!({"task_ids": [task], "completion": "done", "auto_fix": true});
        let run = self
            .jobs
            .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input), None)
            .unwrap();
        self.die(&run.run_id, &head, number);
        self.runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Review),
                    job_run_id: Some(run.run_id.clone()),
                    external_refs: number
                        .map(|number| ExternalRef::github_pr(number.to_string()).unwrap())
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
            )
            .unwrap();
        (task, run.run_id, head)
    }

    /// `run` pushed `head`, opened pull request `number` (when given) and
    /// then lost its worker, as the orphan reconciler finds it.
    fn die(&self, run_id: &str, head: &str, number: Option<u64>) {
        let run = self.jobs.get_job_run(run_id).unwrap().unwrap();
        let mut state = PipelineState::new(
            run.run_id.clone(),
            run.job_id.clone(),
            run.input.clone().unwrap(),
        );
        let mut steps = vec![(
            "push",
            json!({"phase": "push", "local_sha": "0123456789abcdef0123456789abcdef01234567", "branch": head}),
        )];
        if let Some(number) = number {
            steps.push((
                "pr_open",
                json!({"phase": "pr_open", "decision": "performed", "pr_number": number.to_string(), "head": head, "base": "main"}),
            ));
        }
        for (index, (id, output)) in steps.iter().enumerate() {
            state.record_step(
                u32::try_from(index).unwrap(),
                JobRunState::Success,
                Some(output.clone()),
                None,
            );
            state.record_pipeline_output(id, output.clone());
            self.step(run_id, index, id, JobRunState::Success, None);
        }
        self.jobs.write_run_state(run_id, &state).unwrap();
        let mut worker = std::process::Command::new("true").spawn().unwrap();
        worker.wait().unwrap();
        self.jobs
            .mark_job_run_running(run_id, Utc::now(), worker.id())
            .unwrap();
        self.step(
            run_id,
            steps.len(),
            "complete_pr",
            JobRunState::Interrupted,
            Some("worker_terminated"),
        );
        self.jobs
            .finalize_job_run(run_id, JobRunState::Interrupted, Utc::now(), None)
            .unwrap();
    }

    fn step(&self, run: &str, index: usize, id: &str, state: JobRunState, code: Option<&str>) {
        let now = Utc::now();
        self.jobs
            .complete_job_run_step(
                run,
                &JobRunStepParams {
                    lock_wait_ms: None,
                    step_index: index,
                    target_type: JobTargetType::Activity,
                    target_id: id.to_string(),
                    started_at: now,
                    finished_at: now,
                    duration_ms: Some(1),
                    exit_code: None,
                    agent_response_json: None,
                    state,
                    error_code: code.map(str::to_string),
                    error_message: code.map(|_| "worker process exited".to_string()),
                },
            )
            .unwrap();
    }

    /// One running owner drain; each pass names the authority it holds.
    fn drain(&self) -> String {
        let run = self
            .jobs
            .insert_job_run(
                "workspace_auto_pipeline",
                1,
                Utc::now(),
                Some(json!({})),
                None,
            )
            .unwrap();
        self.jobs
            .write_run_state(
                &run.run_id,
                &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
            )
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        run.run_id
    }

    /// The drain's adoption step, as `workspace_auto_pipeline` runs it.
    fn pass(&self, drain: &str, completion: &str, auto_fix: bool) -> Value {
        self.runtime
            .run_deterministic(
                "adopt_orphaned_deliveries",
                &json!({}),
                &json!({
                    "run_id": drain,
                    "max_active_leaf_runs": 4,
                    "completion": completion,
                    "auto_fix": auto_fix,
                }),
                ToolContext::default(),
            )
            .expect("adoption pass")
    }

    fn retries(&self, run: &str) -> Vec<orbit_types::workflow::JobRun> {
        self.jobs.job_run_retries(run, 10).unwrap()
    }
}

fn skipped_reason(pass: &Value, run: &str) -> Option<(String, bool)> {
    pass["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["source_run_id"] == run)
        .map(|entry| {
            (
                entry["reason"].as_str().unwrap().to_string(),
                entry["decided"].as_bool().unwrap(),
            )
        })
}

fn a_dead_leafs_open_pull_request_is_adopted_once_by_a_drain_holding_its_authority() {
    let fx = Fixture::new();
    let (task, run, head) = fx.dead_leaf(Some(17));
    fx.forge.open(17, &head);
    let drain = fx.drain();

    // A drain without `--auto-fix` cannot take over a run granted it; that is
    // no decision, so a drain that holds it adopts the run later.
    let refused = fx.pass(&drain, "done", false);
    assert_eq!(refused["adopted"], json!([]), "{refused}");
    assert_eq!(
        skipped_reason(&refused, &run),
        Some(("authority".to_string(), false)),
        "{refused}"
    );
    assert!(fx.retries(&run).is_empty());

    let adopted = fx.pass(&drain, "done", true);
    let entries = adopted["adopted"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{adopted}");
    assert_eq!(entries[0]["source_run_id"], run, "{adopted}");
    assert_eq!(entries[0]["pr_number"], 17, "{adopted}");
    assert_eq!(entries[0]["task_ids"], json!([task]), "{adopted}");
    let retries = fx.retries(&run);
    assert_eq!(retries.len(), 1, "the interrupted run is resumed once");
    let resumed = &retries[0];
    assert_eq!(resumed.retry_source_run_id.as_deref(), Some(run.as_str()));
    assert_eq!(
        resumed.input.as_ref().unwrap()["auto_fix"],
        true,
        "the resumed run keeps the leaf's auto-fix grant"
    );
    assert_eq!(entries[0]["resumed_run_id"], resumed.run_id, "{adopted}");
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::Review,
        "the task waits on the resumed run"
    );

    let again = fx.pass(&drain, "done", true);
    assert_eq!(again["adopted"], json!([]), "{again}");
    assert_eq!(
        fx.retries(&run).len(),
        1,
        "a later pass adopts nothing more"
    );
}

fn runs_that_no_longer_own_an_open_pull_request_are_left_alone() {
    let fx = Fixture::new();
    // Its pull request was closed or merged after the leaf died.
    let (_, closed, closed_head) = fx.dead_leaf(Some(21));
    // Its task moved on: an operator sent it back to the backlog.
    let (moved_task, moved, moved_head) = fx.dead_leaf(Some(22));
    fx.forge.open(22, &moved_head);
    fx.runtime
        .apply_task_automation_update(
            &moved_task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
        )
        .unwrap();
    // It died before it opened a pull request.
    let (_, unopened, _) = fx.dead_leaf(None);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "done", true);
    assert_eq!(pass["adopted"], json!([]), "{pass}");
    for (run, reason) in [
        (&closed, "pull_request_not_open"),
        (&moved, "task_moved"),
        (&unopened, "no_pull_request"),
    ] {
        assert_eq!(
            skipped_reason(&pass, run),
            Some((reason.to_string(), true)),
            "{pass}"
        );
        assert!(fx.retries(run).is_empty(), "{run} is not resumed");
    }

    // A decision is final: a pull request reopened later is the operator's
    // to resume.
    fx.forge.open(21, &closed_head);
    let later = fx.pass(&drain, "done", true);
    assert_eq!(later["adopted"], json!([]), "{later}");
    assert!(fx.retries(&closed).is_empty());
}

fn a_leaf_that_dies_again_after_its_adoption_is_left_to_an_operator() {
    let fx = Fixture::new();
    let (_, run, head) = fx.dead_leaf(Some(31));
    fx.forge.open(31, &head);
    let drain = fx.drain();
    let first = fx.pass(&drain, "done", true);
    assert_eq!(first["adopted"].as_array().unwrap().len(), 1, "{first}");
    let resumed = fx.retries(&run)[0].run_id.clone();

    // The resumed leaf's worker dies too, past the same pull request.
    fx.die(&resumed, &head, Some(31));
    let again = fx.pass(&drain, "done", true);
    assert_eq!(again["adopted"], json!([]), "{again}");
    assert_eq!(
        skipped_reason(&again, &resumed),
        Some(("resumed_attempt".to_string(), true)),
        "{again}"
    );
    assert!(
        fx.retries(&resumed).is_empty(),
        "the lineage is adopted once"
    );
}

fn store_and_task_errors_skip_one_run_and_never_fail_the_pass() {
    let fx = Fixture::new();
    // A rejected task can be deleted while its dead leaf is still listed.
    let (deleted_task, deleted, deleted_head) = fx.dead_leaf(Some(41));
    fx.forge.open(41, &deleted_head);
    fx.runtime.delete_task(&deleted_task).unwrap();
    // A run whose checkpoints no longer decode.
    let (_, unreadable, unreadable_head) = fx.dead_leaf(Some(42));
    fx.forge.open(42, &unreadable_head);
    fx.runtime
        .sqlite_store()
        .unwrap()
        .connection()
        .lock()
        .unwrap()
        .execute(
            "UPDATE job_run_states SET pipeline_state_json = '{not json' WHERE run_id = ?1",
            rusqlite::params![unreadable],
        )
        .unwrap();
    let drain = fx.drain();

    let pass = fx.pass(&drain, "done", true);
    assert_eq!(pass["adopted"], json!([]), "{pass}");
    assert_eq!(
        skipped_reason(&pass, &deleted),
        Some(("task_moved".to_string(), true)),
        "{pass}"
    );
    assert_eq!(
        skipped_reason(&pass, &unreadable),
        Some(("store_unreadable".to_string(), false)),
        "{pass}"
    );
    // The unreadable run is deferred, not decided: the next pass reads it
    // again, and still answers.
    let next = fx.pass(&drain, "done", true);
    assert_eq!(
        skipped_reason(&next, &unreadable),
        Some(("store_unreadable".to_string(), false)),
        "{next}"
    );
}

fn an_unreachable_forge_is_read_once_per_pass_and_defers_every_run() {
    let fx = Fixture::new();
    let (_, first, first_head) = fx.dead_leaf(Some(51));
    let (_, second, second_head) = fx.dead_leaf(Some(52));
    fx.forge.open(51, &first_head);
    fx.forge.open(52, &second_head);
    fx.forge.0.lock().unwrap().unreachable = true;
    let drain = fx.drain();

    let pass = fx.pass(&drain, "done", true);
    assert_eq!(pass["adopted"], json!([]), "{pass}");
    for run in [&first, &second] {
        assert_eq!(
            skipped_reason(&pass, run),
            Some(("forge_unreadable".to_string(), false)),
            "{pass}"
        );
    }
    assert_eq!(
        fx.forge.0.lock().unwrap().reads,
        1,
        "a pass stops reading the forge after its first failure"
    );

    // Neither run was decided, so a pass after the forge is back adopts.
    fx.forge.0.lock().unwrap().unreachable = false;
    let back = fx.pass(&drain, "done", true);
    assert_eq!(back["adopted"].as_array().unwrap().len(), 2, "{back}");
}

fn an_unloadable_activity_catalog_defers_instead_of_deciding() {
    let fx = Fixture::new();
    let (_, run, head) = fx.dead_leaf(Some(61));
    fx.forge.open(61, &head);
    // An activity file caught mid-edit.
    let broken = fx.activities.join("half_written.yaml");
    std::fs::write(
        &broken,
        "schemaVersion: 2\nkind: Activity\nmetadata: [unclosed\n",
    )
    .unwrap();
    let drain = fx.drain();

    let pass = fx.pass(&drain, "done", true);
    assert_eq!(
        skipped_reason(&pass, &run),
        Some(("resume_unplannable".to_string(), false)),
        "{pass}"
    );
    assert!(fx.retries(&run).is_empty());

    std::fs::remove_file(&broken).unwrap();
    let fixed = fx.pass(&drain, "done", true);
    assert_eq!(fixed["adopted"].as_array().unwrap().len(), 1, "{fixed}");
}

fn host_resource_pressure_holds_adoption() {
    let fx = Fixture::new();
    let (_, run, head) = fx.dead_leaf(Some(71));
    fx.forge.open(71, &head);
    let drain = fx.drain();
    fx.probe.sustain_memory(&fx.runtime, 95.0);

    let pass = fx.pass(&drain, "done", true);
    assert_eq!(pass["adopted"], json!([]), "{pass}");
    assert!(pass["resource_throttle"].is_object(), "{pass}");
    assert!(fx.retries(&run).is_empty(), "nothing starts under pressure");
    assert_eq!(fx.forge.0.lock().unwrap().reads, 0, "{pass}");
}

crate::scenario_table!(
    orphan_adoption_scenarios,
    a_dead_leafs_open_pull_request_is_adopted_once_by_a_drain_holding_its_authority,
    runs_that_no_longer_own_an_open_pull_request_are_left_alone,
    a_leaf_that_dies_again_after_its_adoption_is_left_to_an_operator,
    store_and_task_errors_skip_one_run_and_never_fail_the_pass,
    an_unreachable_forge_is_read_once_per_pass_and_defers_every_run,
    an_unloadable_activity_catalog_defers_instead_of_deciding,
    host_resource_pressure_holds_adoption,
);
