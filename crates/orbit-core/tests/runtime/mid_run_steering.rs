//! Mid-run steering's snapshot watermark: with
//! `workflow.mid_run_steering.enabled`, building an agent's task-context
//! prompt records in the run's state the newest task comment that prompt
//! carried, as its comment-log position and recorded timestamp. Off by
//! default, the prompt and the run's state are untouched; on, the prompt is
//! the same and only the run's state gains the watermark, which never moves
//! backwards and belongs to one run.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::workflow::{CommentLogMark, PipelineState, SteeringWatermark};
use serde_json::{Value, json};
use tempfile::TempDir;

const ENABLED: &str = "[workflow.mid_run_steering]\nenabled = true\n";

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    workspace: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
    task: String,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("config.toml"), config).unwrap();
        let (runtime, jobs) = open(&global, &workspace);
        let task = runtime
            .add_task(TaskAddParams {
                title: "Steering fixture".into(),
                description: "A task an operator comments on after dispatch.".into(),
                acceptance_criteria: vec!["The prompt's comments are recorded.".into()],
                ..Default::default()
            })
            .unwrap()
            .id;
        Self {
            _root: root,
            global,
            workspace,
            runtime,
            jobs,
            task,
        }
    }

    /// Rewrite the workspace config and reopen the runtime over the same
    /// roots, store and task.
    fn reconfigure(&mut self, config: &str) {
        std::fs::write(self.workspace.join("config.toml"), config).unwrap();
        (self.runtime, self.jobs) = open(&self.global, &self.workspace);
    }

    /// A running pipeline with persisted state, as the engine seeds it.
    fn run(&self, retry_of: Option<&str>) -> String {
        let run = self
            .jobs
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({ "task_id": self.task })),
                retry_of.map(str::to_string),
            )
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
        self.jobs.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }

    /// Append a comment and return its recorded timestamp.
    fn comment(&self, message: &str) -> DateTime<Utc> {
        self.runtime
            .update_task_with_identity(
                &self.task,
                TaskUpdateParams {
                    comment: Some(message.into()),
                    ..Default::default()
                },
                Some("claude".into()),
                None,
            )
            .unwrap();
        self.runtime
            .get_task_comments(&self.task)
            .unwrap()
            .last()
            .unwrap()
            .at
    }

    /// The task context `step` of `run` embeds in its agent prompt.
    fn prompt(&self, run: &str, step: &str) -> Value {
        RuntimeHost::task_context_for_agent_input(
            &self.runtime,
            run,
            step,
            &json!({ "task_id": self.task }),
        )
        .unwrap()
        .expect("a task context")
    }

    fn state(&self, run: &str) -> PipelineState {
        self.jobs.read_run_state(run).unwrap().unwrap()
    }

    fn watermark(&self, run: &str) -> SteeringWatermark {
        self.state(run)
            .steering_watermarks
            .remove(&self.task)
            .expect("the run holds a watermark for its task")
    }

    fn seed_watermark(&self, run: &str, step: &str, mark: CommentLogMark) {
        let mut state = self.state(run);
        state.record_steering_snapshot(&self.task, step, Some(mark));
        self.jobs.write_run_state(run, &state).unwrap();
    }
}

fn open(global: &Path, workspace: &Path) -> (OrbitRuntime, Arc<dyn JobRunStoreBackend>) {
    let runtime = OrbitRuntime::from_roots(global, workspace).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    (runtime, jobs)
}

fn mark(position: u64, at: DateTime<Utc>) -> Option<CommentLogMark> {
    Some(CommentLogMark { position, at })
}

fn steering_off_writes_nothing_and_on_leaves_the_prompt_unchanged() {
    let mut fx = Fixture::new("");
    fx.comment("Use the existing helper.");
    fx.comment("Also cover the error path.");
    let run = fx.run(None);
    let state_before = serde_json::to_string(&fx.state(&run)).unwrap();
    let task_before = serde_json::to_string(&fx.runtime.get_task(&fx.task).unwrap()).unwrap();

    let off = serde_json::to_string(&fx.prompt(&run, "implement")).unwrap();
    assert_eq!(
        serde_json::to_string(&fx.state(&run)).unwrap(),
        state_before,
        "with steering off, building a prompt writes nothing to the run's state"
    );
    assert_eq!(
        serde_json::to_string(&fx.runtime.get_task(&fx.task).unwrap()).unwrap(),
        task_before,
        "nor to the task"
    );

    fx.reconfigure(ENABLED);
    let on = serde_json::to_string(&fx.prompt(&run, "implement")).unwrap();
    assert_eq!(
        on, off,
        "the watermark is recorded, never carried in the prompt"
    );
    assert!(fx.state(&run).steering_watermarks.contains_key(&fx.task));
    assert_eq!(
        serde_json::to_string(&fx.runtime.get_task(&fx.task).unwrap()).unwrap(),
        task_before,
        "the watermark lives on the run, not the task"
    );
}

fn steering_on_records_the_newest_comment_by_position_and_never_moves_back() {
    let fx = Fixture::new(ENABLED);
    let run = fx.run(None);

    fx.prompt(&run, "implement");
    let empty = fx.watermark(&run);
    assert_eq!(empty.comment, None, "a prompt without comments names none");
    assert_eq!(empty.steps["implement"].comment, None);

    let first = fx.comment("Use the existing helper.");
    fx.prompt(&run, "implement");
    let second = fx.comment("Also cover the error path.");
    fx.prompt(&run, "review");
    let watermark = fx.watermark(&run);
    assert_eq!(watermark.comment, mark(1, second));
    assert_eq!(watermark.steps["implement"].comment, mark(0, first));
    assert_eq!(watermark.steps["review"].comment, mark(1, second));

    // A recorded mark sharing the newest comment's timestamp but at an
    // earlier position is still behind it: identity is the position.
    let other = fx.run(None);
    fx.seed_watermark(
        &other,
        "implement",
        CommentLogMark {
            position: 0,
            at: second,
        },
    );
    fx.prompt(&other, "implement");
    assert_eq!(fx.watermark(&other).comment, mark(1, second));

    // A prompt carrying less than the run already recorded, as a stale
    // replica read would, leaves the watermark where it was.
    let ahead = CommentLogMark {
        position: 7,
        at: first,
    };
    fx.seed_watermark(&run, "implement", ahead);
    fx.prompt(&run, "implement");
    let watermark = fx.watermark(&run);
    assert_eq!(watermark.comment, Some(ahead));
    assert_eq!(watermark.steps["implement"].comment, Some(ahead));
}

fn two_runs_of_one_task_hold_independent_watermarks() {
    let fx = Fixture::new(ENABLED);
    let first_run = fx.run(None);
    let first = fx.comment("Use the existing helper.");
    fx.prompt(&first_run, "implement");

    let retry = fx.run(Some(&first_run));
    let second = fx.comment("Also cover the error path.");
    fx.prompt(&retry, "implement");

    assert_eq!(fx.watermark(&first_run).comment, mark(0, first));
    assert_eq!(fx.watermark(&retry).comment, mark(1, second));

    // A dispatch outside any persisted run has no record to hold one.
    fx.prompt("jrun-without-state", "implement");
}

crate::scenario_table!(
    mid_run_steering_watermark_scenarios,
    steering_off_writes_nothing_and_on_leaves_the_prompt_unchanged,
    steering_on_records_the_newest_comment_by_position_and_never_moves_back,
    two_runs_of_one_task_hold_independent_watermarks
);
