#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

//! [ORB-14617] The clock retries a run held because the forge refused its
//! push: each tick inside the retry window resumes it once, a run held for
//! another reason is left alone, and past the window the clock stops and
//! blocks the run's in-progress task for a human, whose resume re-admits it.
//! Expiry acts once and leaves tasks admitted under an unrelated run alone.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
use tracing_subscriber::prelude::*;

struct RunEvents {
    run_id: String,
    count: Arc<AtomicUsize>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RunEvents {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Visitor<'a> {
            run_id: &'a str,
            matches: bool,
        }
        impl tracing::field::Visit for Visitor<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if matches!(field.name(), "run_id" | "source_run_id")
                    && format!("{value:?}") == self.run_id
                {
                    self.matches = true;
                }
            }
        }
        if event.metadata().target() == "orbit.core.sweep" {
            let mut visitor = Visitor {
                run_id: &self.run_id,
                matches: false,
            };
            event.record(&mut visitor);
            if visitor.matches {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

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
        // Submitted runs execute in-process. The substitute child test
        // stays alive while the fixture does, so the supervisor never sees a
        // resumed run's worker exit before its run starts and interrupts the
        // run (blocking its task) while the test is still asserting.
        crate::worker_fixture::install(root.path(), "removed");
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

        self.hold_run(&run.run_id, held_since);
        (task, run.run_id)
    }

    fn hold_run(&self, run_id: &str, held_since: Option<DateTime<Utc>>) {
        let run = self.jobs.get_job_run(run_id).unwrap().unwrap();
        // Its worker has exited, as a finished run's has.
        let mut worker = Command::new("true").spawn().unwrap();
        worker.wait().unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), worker.id())
            .unwrap();

        let mut state = self
            .jobs
            .read_run_state(run_id)
            .unwrap()
            .unwrap_or_else(|| {
                PipelineState::new(run.run_id.clone(), run.job_id, run.input.unwrap())
            });
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
    }

    fn admit_unrelated_run(&self, task: &str) -> String {
        let run = self
            .jobs
            .insert_job_run(
                "test_pipeline",
                1,
                Utc::now(),
                Some(json!({"task_ids": [task]})),
                None,
            )
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        self.runtime
            .apply_task_automation_update(
                task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::InProgress),
                    job_run_id: Some(run.run_id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        run.run_id
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
    // A real resumed delivery keeps the original batch binding through its
    // successful worktree checkpoint, even though the retry owns cleanup.
    fx.jobs
        .update_run_state(&run, &mut |_, state| {
            state.record_step(
                0,
                JobRunState::Success,
                Some(json!({"job_run_id": run})),
                None,
            );
            Ok(())
        })
        .unwrap();

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
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().job_run_id.as_deref(),
        Some(run.as_str())
    );
    let resumed_state = fx.jobs.read_run_state(&invoke.run_id).unwrap().unwrap();
    assert!(
        resumed_state.forge_hold_expired_at.is_none(),
        "expiry belongs to the source hold"
    );

    // If a manually resumed push holds again, that attempt expires once too,
    // using its durable coupling rather than the older checkpoint binding.
    fx.hold_run(&invoke.run_id, Some(Utc::now() - Duration::hours(3)));
    fx.tick();
    assert_eq!(fx.status(&task), TaskStatus::Blocked);
    let retry_history = fx.runtime.get_task_history(&task).unwrap();
    let entry = retry_history.last().unwrap();
    assert_eq!(entry.event, FORGE_UNAVAILABLE_EXPIRED_EVENT);
    assert!(
        entry
            .note
            .as_deref()
            .unwrap()
            .contains(&format!("run={};", invoke.run_id))
    );
    fx.tick();
    assert_eq!(
        fx.runtime.get_task_history(&task).unwrap().len(),
        retry_history.len()
    );
}

#[test]
fn expired_hold_does_not_block_or_log_again_after_readmission() {
    if run_isolated_test(
        "forge_hold_resume::expired_hold_does_not_block_or_log_again_after_readmission",
    ) {
        return;
    }
    for unrelated_run in [true, false] {
        let fx = Fixture::new();
        let (task, run) = fx.held_delivery(Some(Utc::now() - Duration::hours(3)));
        let events = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(RunEvents {
            run_id: run.clone(),
            count: events.clone(),
        });
        let _subscriber = tracing::subscriber::set_default(subscriber);
        fx.tick();
        assert_eq!(fx.status(&task), TaskStatus::Blocked);
        assert_eq!(events.load(Ordering::SeqCst), 1, "one expiry diagnostic");
        let expired_state =
            serde_json::to_value(fx.jobs.read_run_state(&run).unwrap().unwrap()).unwrap();
        assert!(!expired_state["forge_hold_expired_at"].is_null());

        fx.runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                },
            )
            .unwrap();
        let current_run = if unrelated_run {
            fx.admit_unrelated_run(&task)
        } else {
            fx.runtime
                .apply_task_automation_update(
                    &task,
                    TaskAutomationUpdate {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                )
                .unwrap();
            run.clone()
        };
        let admitted_task = serde_json::to_value(fx.runtime.get_task(&task).unwrap()).unwrap();
        let admitted_run =
            serde_json::to_value(fx.jobs.get_job_run(&current_run).unwrap().unwrap()).unwrap();
        let history = serde_json::to_value(fx.runtime.get_task_history(&task).unwrap()).unwrap();
        for _ in 0..2 {
            fx.tick();
            assert_eq!(
                serde_json::to_value(fx.runtime.get_task(&task).unwrap()).unwrap(),
                admitted_task
            );
            assert_eq!(
                serde_json::to_value(fx.jobs.get_job_run(&current_run).unwrap().unwrap()).unwrap(),
                admitted_run
            );
            assert_eq!(
                serde_json::to_value(fx.runtime.get_task_history(&task).unwrap()).unwrap(),
                history
            );
            assert_eq!(
                serde_json::to_value(fx.jobs.read_run_state(&run).unwrap().unwrap()).unwrap(),
                expired_state
            );
            assert_eq!(
                events.load(Ordering::SeqCst),
                1,
                "expired hold is not logged again"
            );
            assert_eq!(fx.retries(&run), 0);
        }
        if !unrelated_run {
            // Simulate interruption after the task event committed but before
            // the run acknowledgement: history must still prevent a second block.
            fx.jobs
                .update_run_state(&run, &mut |_, state| {
                    state.forge_hold_expired_at = None;
                    Ok(())
                })
                .unwrap();
            fx.tick();
            assert_eq!(
                serde_json::to_value(fx.runtime.get_task(&task).unwrap()).unwrap(),
                admitted_task
            );
            assert_eq!(
                serde_json::to_value(fx.runtime.get_task_history(&task).unwrap()).unwrap(),
                history
            );
            assert_eq!(events.load(Ordering::SeqCst), 1);
            assert!(
                fx.jobs
                    .read_run_state(&run)
                    .unwrap()
                    .unwrap()
                    .forge_hold_expired_at
                    .is_some()
            );
        }
    }
}

#[test]
fn expiry_leaves_a_task_admitted_under_an_unrelated_run_alone() {
    if run_isolated_test(
        "forge_hold_resume::expiry_leaves_a_task_admitted_under_an_unrelated_run_alone",
    ) {
        return;
    }
    let fx = Fixture::new();
    let (task, held_run) = fx.held_delivery(Some(Utc::now() - Duration::hours(3)));
    let current_run = fx.admit_unrelated_run(&task);
    let task_before = serde_json::to_value(fx.runtime.get_task(&task).unwrap()).unwrap();
    let run_before =
        serde_json::to_value(fx.jobs.get_job_run(&current_run).unwrap().unwrap()).unwrap();
    let history_before = serde_json::to_value(fx.runtime.get_task_history(&task).unwrap()).unwrap();

    fx.tick();
    fx.tick();

    assert_eq!(
        serde_json::to_value(fx.runtime.get_task(&task).unwrap()).unwrap(),
        task_before
    );
    assert_eq!(
        serde_json::to_value(fx.jobs.get_job_run(&current_run).unwrap().unwrap()).unwrap(),
        run_before
    );
    assert_eq!(
        serde_json::to_value(fx.runtime.get_task_history(&task).unwrap()).unwrap(),
        history_before
    );
    assert!(
        fx.jobs
            .read_run_state(&held_run)
            .unwrap()
            .unwrap()
            .forge_hold_expired_at
            .is_some()
    );
    assert_eq!(fx.retries(&held_run), 0);
}
