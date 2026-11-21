//! A running local auto drain (`workspace_auto_pipeline`) picks up edits to
//! `workflow.<tier>_complexity_crews` on its next admission in the same worker,
//! excludes removed members, records the current pool in the new child run
//! input, and keeps any tier supplied as a run override at its launch members.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_core::{DrainAdmissionsStopRequest, OrbitRuntime, Task, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};
use tempfile::TempDir;

const LEAF_JOB: &str = "task_auto_pipeline";

struct LocalDrainFixture {
    _root: TempDir,
    repo: PathBuf,
    workspace: PathBuf,
    runtime: OrbitRuntime,
}

impl LocalDrainFixture {
    fn new(config: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        let workspace = repo.join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("config.toml"), config).unwrap();

        let git = |args: &[&str]| {
            let mut command = crate::git_test_support::command();
            let output = command.args(args).current_dir(&repo).output().unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Orbit Test"][..],
            &["config", "user.email", "orbit-test@example.com"][..],
            &["config", "commit.gpgsign", "false"][..],
        ] {
            git(args);
        }
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "seed"]);

        init_workspace_at_root(
            &global,
            InitOptions {
                global_only: true,
                refresh_defaults: true,
                ..Default::default()
            },
        )
        .unwrap();

        let runtime = OrbitRuntime::from_roots(&global, &workspace)
            .unwrap()
            .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm())
            .with_automation_machine_identity(Some("fixture-machine".into()));
        Self {
            _root: root,
            repo,
            workspace,
            runtime,
        }
    }

    fn write_config(&self, config: &str) {
        std::fs::write(self.workspace.join("config.toml"), config).unwrap();
    }

    fn backlog_task(&self, name: &str, complexity: TaskComplexity) -> Task {
        let file = format!("{name}.md");
        std::fs::write(self.repo.join(&file), "fixture\n").unwrap();
        let mut add = crate::git_test_support::command();
        assert!(
            add.args(["add", &file])
                .current_dir(&self.repo)
                .status()
                .unwrap()
                .success()
        );
        let mut commit = crate::git_test_support::command();
        assert!(
            commit
                .args(["commit", "-m", name])
                .current_dir(&self.repo)
                .status()
                .unwrap()
                .success()
        );
        self.runtime
            .add_task(TaskAddParams {
                title: name.into(),
                description: format!("Ship {name}."),
                acceptance_criteria: vec!["The change is in place.".into()],
                plan: format!("Edit {file}."),
                status: Some(TaskStatus::Backlog),
                context_files: vec![format!("file:{file}")],
                complexity,
                ..Default::default()
            })
            .unwrap()
    }

    fn leaf_for(&self, drain: &str, task: &Task) -> Option<JobRun> {
        let state = self.runtime.read_run_state(drain).unwrap()?;
        state
            .child_dispatches
            .into_iter()
            .filter(|child| child.job_name == LEAF_JOB)
            .filter_map(|child| self.runtime.show_job_run(&child.child_run_id).ok())
            .find(|run| {
                run.input.as_ref().is_some_and(|input| {
                    input["task_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.contains(&json!(task.id)))
                })
            })
    }

    fn wait_for_leaf(&self, drain: &str, task: &Task) -> JobRun {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(leaf) = self.leaf_for(drain, task) {
                return leaf;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for leaf for {} in drain {drain}",
                task.id
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn settle_leaf(&self, leaf_run_id: &str, task: &Task) {
        self.runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Archived),
                    ..Default::default()
                },
            )
            .unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.runtime.workspace_id().unwrap(),
        );
        if jobs
            .get_job_run(leaf_run_id)
            .unwrap()
            .is_some_and(|run| run.state == JobRunState::Pending)
        {
            jobs.mark_job_run_running(leaf_run_id, Utc::now(), std::process::id())
                .unwrap();
        }
        jobs.finalize_job_run(leaf_run_id, JobRunState::Success, Utc::now(), None)
            .unwrap();
    }
}

fn config_with_pools(low: &str, medium: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"sol\"\nlow_complexity_crews = {low}\n\
         medium_complexity_crews = {medium}\n\n\
         [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n\n\
         [crews.sonnet]\nprovider = \"claude\"\nmodel = \"sonnet-model\"\n\n\
         [crews.neon]\nprovider = \"grok\"\nmodel = \"neon-model\"\n"
    )
}

#[test]
fn running_local_auto_drain_refreshes_config_tiers_and_keeps_run_overrides() {
    if !crate::dispatch_admission::isolated(
        "drain_crew_pools::running_local_auto_drain_refreshes_config_tiers_and_keeps_run_overrides",
    ) {
        return;
    }
    let fixture = LocalDrainFixture::new(&config_with_pools("[\"sonnet\"]", "[\"sonnet\"]"));
    let release = Path::new(fixture._root.path()).join("release-workers");
    let mut owned_workers =
        crate::worker_fixture::install_owned_workers(&release, fixture.repo.clone());

    let task_a = fixture.backlog_task("pool-initial", TaskComplexity::Medium);

    let drain = fixture
        .runtime
        .submit_pipeline_run(
            "workspace_auto_pipeline",
            json!({
                "for_seconds": 3600,
                "max_active_leaf_runs": 1,
                "completion": "review",
                "approve_proposed": false,
                "poll_sleep_seconds": 0,
                "idle_sleep_seconds": 1,
                "low_complexity_crews": ["sonnet"],
            }),
            None,
            None,
        )
        .unwrap()
        .run_id;
    let executor = {
        let runtime = fixture.runtime.clone();
        let drain = drain.clone();
        std::thread::spawn(move || runtime.execute_pipeline_run_worker(&drain))
    };

    let leaf_a = fixture.wait_for_leaf(&drain, &task_a);
    let input_a: Value = leaf_a.input.clone().unwrap();
    assert_eq!(input_a["crew"], "sonnet");
    assert_eq!(
        input_a["auto_crew_pools"]["medium"],
        json!({"crews": [{"name": "sonnet", "weight": 1}], "source": "workflow.medium_complexity_crews"})
    );
    assert_eq!(
        input_a["auto_crew_pools"]["low"],
        json!({"crews": [{"name": "sonnet", "weight": 1}], "source": "run_input.low_complexity_crews"})
    );

    // Add the second medium task while `sonnet` is still configured, so it
    // starts with `sonnet` on its record and waits behind `leaf_a`'s slot.
    let task_b = fixture.backlog_task("pool-refreshed", TaskComplexity::Medium);
    assert_eq!(task_b.crew.as_deref(), Some("sonnet"));

    // Edit config while the drain worker stays open: remove `sonnet` and add `neon`.
    fixture.write_config(&config_with_pools("[\"neon\"]", "[\"neon\"]"));
    fixture.settle_leaf(&leaf_a.run_id, &task_a);

    let leaf_b = fixture.wait_for_leaf(&drain, &task_b);
    let input_b: Value = leaf_b.input.clone().unwrap();
    assert_eq!(
        input_b["crew"], "neon",
        "the running drain must exclude the removed member and select the edited pool: {input_b}"
    );
    assert_eq!(
        input_b["auto_crew_pools"]["medium"],
        json!({"crews": [{"name": "neon", "weight": 1}], "source": "workflow.medium_complexity_crews"}),
        "child run input must record the current config pool: {input_b}"
    );
    assert_eq!(
        input_b["crew_selection"]["eligible_pool"],
        json!([{"name": "neon", "weight": 1}]),
        "selection must exclude the removed member: {input_b}"
    );

    // A low-complexity task admitted after the same config change still uses
    // the drain's launch-time run override for `low_complexity_crews`.
    let task_c = fixture.backlog_task("pool-override", TaskComplexity::Low);
    fixture.settle_leaf(&leaf_b.run_id, &task_b);

    let leaf_c = fixture.wait_for_leaf(&drain, &task_c);
    let input_c: Value = leaf_c.input.clone().unwrap();
    assert_eq!(
        input_c["crew"], "sonnet",
        "a run-override tier must retain its launch members across the config edit: {input_c}"
    );
    assert_eq!(
        input_c["auto_crew_pools"]["low"],
        json!({"crews": [{"name": "sonnet", "weight": 1}], "source": "run_input.low_complexity_crews"}),
        "child run input must retain the launch override pool: {input_c}"
    );
    assert_eq!(
        input_c["crew_selection"]["eligible_pool"],
        json!([{"name": "sonnet", "weight": 1}]),
        "selection must retain the launch override members: {input_c}"
    );
    assert_eq!(
        input_c["auto_crew_pools"]["medium"],
        json!({"crews": [{"name": "neon", "weight": 1}], "source": "workflow.medium_complexity_crews"})
    );

    fixture.settle_leaf(&leaf_c.run_id, &task_c);
    fixture
        .runtime
        .stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
            actor: "operator",
            source: "fixture",
            reason: None,
            claim_token: None,
            force: false,
        })
        .unwrap();
    executor.join().unwrap().unwrap();
    let mut expected_workers = vec![drain.clone()];
    expected_workers.extend(
        fixture
            .runtime
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .child_dispatches
            .into_iter()
            .map(|child| child.child_run_id),
    );
    owned_workers.release_and_wait(&expected_workers);
}
