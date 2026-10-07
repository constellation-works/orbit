#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

//! [ORB-14617] The clock retries a run held because the forge refused its
//! push: each tick inside the retry window resumes it once, a run held for
//! another reason is left alone, and past the window the clock stops and
//! blocks the run's in-progress task for a human, whose resume re-admits it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use orbit_core::application::routines::{
    RoutineMachineIdentity, SweepOptions, run_sweep_at_with_providers,
};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitError, OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::{JobRunStepParams, JobRunStoreBackend};
use orbit_types::workflow::{
    FORGE_UNAVAILABLE_ERROR_CODE, FORGE_UNAVAILABLE_EXPIRED_EVENT, ForgeUnavailableHold,
    JobRunState, JobTargetType, PipelineState,
};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::json;
use tempfile::TempDir;

struct SingleWorkspace(OrbitRuntime);

impl RoutineWorkspaceProvider for SingleWorkspace {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let workspace = Workspace {
            id: self.0.workspace_id()?,
            name: "test-workspace".into(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok(DiscoveredWorkspaces {
            entries: vec![(workspace, self.0.clone())],
            ..DiscoveredWorkspaces::default()
        })
    }
}

fn run_isolated_test(test_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_FORGE_HOLD_RESUME_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }

    let home = TempDir::new().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    true
}

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        // This test binary cannot be re-executed as a worker. The substitute
        // stays alive while the fixture does, so the supervisor never sees a
        // resumed run's worker exit before its run starts and interrupts the
        // run (blocking its task) while the test is still asserting.
        orbit_core::test_support::install_substitute_pipeline_worker([
            "sh".to_string(),
            "-c".to_string(),
            "i=0; while [ -d \"$1\" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done"
                .to_string(),
            "worker".to_string(),
            root.path().to_string_lossy().into_owned(),
        ]);
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            jobs_dir.join("test_pipeline.yaml"),
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: test_pipeline\nspec:\n  state: enabled\n  steps: []\n",
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            global,
            runtime,
            jobs,
        }
    }

    /// An in-progress task whose delivery run ended held: with a forge hold
    /// first held at `held_since`, or, without one, for another reason.
    fn held_delivery(&self, held_since: Option<DateTime<Utc>>) -> (String, String) {
        let task = self
            .runtime
            .add_task(TaskAddParams {
                title: "Forge hold fixture".to_string(),
                description: "A task whose delivery push the forge refused.".to_string(),
                acceptance_criteria: vec!["Delivered.".to_string()],
                plan: "1. Deliver it.".to_string(),
                complexity: TaskComplexity::Medium,
                task_type: Some(TaskType::Bug),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id;
        let input = json!({ "task_ids": [task] });
        let run = self
            .jobs
            .insert_job_run("test_pipeline", 1, Utc::now(), Some(input.clone()), None)
            .unwrap();
        self.runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::InProgress),
                    job_run_id: Some(run.run_id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();

        // Its worker has exited, as a finished run's has.
        let mut worker = Command::new("true").spawn().unwrap();
        worker.wait().unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), worker.id())
            .unwrap();

        let mut state = PipelineState::new(run.run_id.clone(), run.job_id, input);
        state.forge_hold = held_since.map(|held_since| ForgeUnavailableHold {
            target_ref: "refs/heads/orbit/candidate".to_string(),
            head_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            attempts: 6,
            waited_ms: 300_000,
            diagnostic: "! [remote rejected] orbit/candidate -> orbit/candidate \
                         (Internal Server Error)"
                .to_string(),
            step_id: "push".to_string(),
            held_at: held_since,
            held_since,
        });
        self.jobs.write_run_state(&run.run_id, &state).unwrap();
        let now = Utc::now();
        self.jobs
            .complete_job_run_step(
                &run.run_id,
                &JobRunStepParams {
                    step_index: 1,
                    target_type: JobTargetType::Activity,
                    target_id: "diagnostic".to_string(),
                    started_at: now,
                    finished_at: now,
                    duration_ms: Some(1),
                    exit_code: None,
                    agent_response_json: None,
                    state: JobRunState::Held,
                    error_code: Some(
                        held_since
                            .map_or("review_evidence_pending", |_| FORGE_UNAVAILABLE_ERROR_CODE)
                            .to_string(),
                    ),
                    error_message: Some("held".to_string()),
                },
            )
            .unwrap();
        self.jobs
            .finalize_job_run(&run.run_id, JobRunState::Held, now, None)
            .unwrap();
        (task, run.run_id)
    }

    fn tick(&self) {
        let sweep = run_sweep_at_with_providers(
            &self.global,
            SweepOptions::default(),
            RoutineMachineIdentity {
                machine_id: "test-mach".into(),
                machine_name: "test-host".into(),
            },
            &SingleWorkspace(self.runtime.clone()),
        )
        .expect("sweep runs");
        assert!(!sweep.lock_busy);
    }

    fn retries(&self, run: &str) -> usize {
        self.jobs.job_run_retries(run, 10).unwrap().len()
    }

    fn status(&self, task: &str) -> TaskStatus {
        self.runtime.get_task(task).unwrap().status
    }
}

#[test]
fn the_clock_resumes_a_forge_held_run_once_per_hold_inside_the_window() {
    if run_isolated_test(
        "forge_hold_resume::the_clock_resumes_a_forge_held_run_once_per_hold_inside_the_window",
    ) {
        return;
    }
    let fx = Fixture::new();
    let (forge_task, forge_run) = fx.held_delivery(Some(Utc::now() - Duration::minutes(30)));
    let (other_task, other_run) = fx.held_delivery(None);

    fx.tick();

    assert_eq!(fx.retries(&forge_run), 1, "the forge-held run is resumed");
    let resumed = &fx.jobs.job_run_retries(&forge_run, 1).unwrap()[0];
    assert_eq!(
        resumed.retry_source_run_id.as_deref(),
        Some(forge_run.as_str())
    );
    assert_eq!(fx.retries(&other_run), 0, "a run held for another reason");
    assert_eq!(fx.status(&forge_task), TaskStatus::InProgress);
    assert_eq!(fx.status(&other_task), TaskStatus::InProgress);

    fx.tick();
    assert_eq!(
        fx.retries(&forge_run),
        1,
        "the retry is the lineage's attempt now; the source is not resumed again"
    );
}

#[test]
fn past_the_window_the_clock_blocks_the_task_and_a_manual_resume_readmits_it() {
    if run_isolated_test(
        "forge_hold_resume::past_the_window_the_clock_blocks_the_task_and_a_manual_resume_readmits_it",
    ) {
        return;
    }
    let fx = Fixture::new();
    let (task, run) = fx.held_delivery(Some(Utc::now() - Duration::hours(3)));

    fx.tick();

    assert_eq!(fx.retries(&run), 0, "the clock stops retrying");
    assert_eq!(fx.status(&task), TaskStatus::Blocked);
    let history = fx.runtime.get_task_history(&task).unwrap();
    let entry = history.last().unwrap();
    assert_eq!(entry.event, FORGE_UNAVAILABLE_EXPIRED_EVENT, "{entry:?}");
    let note = entry.note.as_deref().unwrap();
    assert!(note.contains(&format!("run={run};")), "{note}");
    assert!(
        note.contains("0123456789abcdef"),
        "names the held head: {note}"
    );

    fx.tick();
    assert_eq!(fx.retries(&run), 0);
    assert_eq!(
        fx.runtime.get_task_history(&task).unwrap().len(),
        history.len()
    );

    let invoke = fx
        .runtime
        .submit_resume_run(&run, Some("operator"), None)
        .expect("a forge-held run is resumable");
    assert_eq!(fx.retries(&run), 1);
    assert_eq!(fx.status(&task), TaskStatus::InProgress);
    let readmitted = fx.runtime.get_task_history(&task).unwrap();
    let entry = readmitted.last().unwrap();
    assert_eq!(entry.event, "resume_readmitted", "{entry:?}");
    assert!(
        entry.note.as_deref().unwrap().contains(&invoke.run_id),
        "{entry:?}"
    );
}
