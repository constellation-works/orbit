//! Mid-run steering's snapshot watermark and safe-point delivery: with
//! `workflow.mid_run_steering.enabled`, building an agent's task-context
//! prompt records in the run's state the newest task comment that prompt
//! carried, as its comment-log position and recorded timestamp. Off by
//! default, the prompt and the run's state are untouched; on, the run's
//! first prompt is the same and only the run's state gains the watermark,
//! which never moves backwards and belongs to one run. Every later prompt of
//! the run leads with one bounded steering block of the operator comments
//! past the watermark, records a `steering_delivered` task history event and
//! advances the watermark past what it carried in full.

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

    /// Append an operator's comment and return its recorded timestamp.
    fn operator_comment(&self, message: &str) -> DateTime<Utc> {
        self.runtime
            .update_task_as_human(
                &self.task,
                TaskUpdateParams {
                    comment: Some(message.into()),
                    ..Default::default()
                },
                "human:operator".into(),
            )
            .unwrap();
        self.runtime
            .get_task_comments(&self.task)
            .unwrap()
            .last()
            .unwrap()
            .at
    }

    /// The `steering_delivered` events on the task, as their notes.
    fn deliveries(&self) -> Vec<Value> {
        self.runtime
            .get_task_history(&self.task)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.event == "steering_delivered")
            .map(|entry| {
                assert_eq!(entry.by, "system", "only the host records a delivery");
                serde_json::from_str(entry.note.as_deref().unwrap()).unwrap()
            })
            .collect()
    }

    /// Append an agent's comment and return its recorded timestamp.
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

/// The steering block a prompt leads with, checked to be its first key.
fn steering(prompt: &Value) -> Option<&Value> {
    let steering = prompt.get("steering")?;
    assert_eq!(
        prompt
            .as_object()
            .unwrap()
            .keys()
            .next()
            .map(String::as_str),
        Some("steering"),
        "the block leads the frozen task context"
    );
    Some(steering)
}

fn marks(note: &Value, key: &str) -> Vec<u64> {
    note[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|mark| mark["position"].as_u64().unwrap())
        .collect()
}

fn a_safe_point_delivers_operator_comments_once_oldest_first() {
    let mut fx = Fixture::new(ENABLED);
    let before = fx.operator_comment("Before dispatch: keep the public API.");
    let run = fx.run(None);
    let snapshot = fx.prompt(&run, "implement");
    assert!(
        snapshot.get("steering").is_none(),
        "the run's first prompt is its snapshot"
    );

    // Operator guidance interleaved with Orbit's own bookkeeping: an
    // automation comment, a history-only write and an agent's comment.
    fx.operator_comment("STEER-ONE: name the config key `max_rounds`.");
    fx.runtime
        .update_task_as_human(
            &fx.task,
            TaskUpdateParams {
                comment: Some("PILOT-NOISE: assessment refreshed.".into()),
                ..Default::default()
            },
            "task-pilot".into(),
        )
        .unwrap();
    fx.runtime
        .update_task_as_human(
            &fx.task,
            TaskUpdateParams {
                plan: Some("HISTORY-NOISE: a plan edit.".into()),
                ..Default::default()
            },
            "human:operator".into(),
        )
        .unwrap();
    fx.comment("AGENT-NOISE: implementation started.");
    let second = fx.operator_comment("STEER-TWO: also cover the error path.");

    let recovery = fx.prompt(&run, "step_failure_recovery");
    let block = steering(&recovery).expect("a safe point with pending steering");
    assert_eq!(block["run_id"], json!(run));
    assert_eq!(block["snapshot"], json!(mark(0, before)));
    let text = block["comments"].as_str().unwrap();
    let one = text.find("STEER-ONE").expect("the older comment");
    let two = text.find("STEER-TWO").expect("the newer comment");
    assert!(one < two, "oldest first: {text}");
    for excluded in [
        "Before dispatch",
        "PILOT-NOISE",
        "HISTORY-NOISE",
        "AGENT-NOISE",
    ] {
        assert!(
            !text.contains(excluded),
            "only post-snapshot operator comments steer, not {excluded}: {text}"
        );
    }
    let deliveries = fx.deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["run_id"], json!(run));
    assert_eq!(deliveries[0]["step_id"], json!("step_failure_recovery"));
    assert_eq!(marks(&deliveries[0], "delivered"), [1, 4]);
    assert_eq!(marks(&deliveries[0], "truncated"), Vec::<u64>::new());
    assert_eq!(fx.watermark(&run).comment, mark(4, second));

    // A safe point with nothing new builds the prompt as steering off does.
    let quiet = fx.prompt(&run, "final_recovery");
    fx.reconfigure("");
    let off = fx.prompt(&run, "final_recovery");
    assert_eq!(quiet, off, "no empty block");
    assert_eq!(fx.deliveries().len(), 1, "and no event");

    // Off, a later comment is neither delivered nor recorded.
    fx.operator_comment("STEER-THREE: written while steering is off.");
    let off = fx.prompt(&run, "final_recovery");
    assert!(off.get("steering").is_none());
    assert_eq!(fx.deliveries().len(), 1);
    assert_eq!(fx.watermark(&run).comment, mark(4, second));
}

fn a_cut_block_advances_only_past_comments_it_carried_in_full() {
    let fx = Fixture::new(ENABLED);
    let run = fx.run(None);
    fx.prompt(&run, "implement");

    fx.operator_comment("STEER-FIRST: short.");
    fx.operator_comment(&format!("STEER-LONG-A {}", "a".repeat(20 * 1024)));
    let filler = "b".repeat(10 * 1024);
    fx.operator_comment(&format!("STEER-LONG-B {filler} MID-B {filler}"));
    let last = fx.operator_comment("STEER-LAST: short.");

    let block = steering(&fx.prompt(&run, "validate_repair"))
        .unwrap()
        .clone();
    let text = block["comments"].as_str().unwrap();
    assert!(
        text.len() <= 32 * 1024,
        "the block is bounded: {} B",
        text.len()
    );
    assert!(
        text.contains(&format!("orbit task show {}", fx.task)),
        "the marker says where to read every comment"
    );
    assert!(text.contains("STEER-FIRST") && text.contains("STEER-LAST"));
    assert!(!text.contains("MID-B"), "the middle comment is cut");
    let deliveries = fx.deliveries();
    assert_eq!(marks(&deliveries[0], "delivered"), [0, 1, 3]);
    assert_eq!(marks(&deliveries[0], "truncated"), [2]);
    let watermark = fx.watermark(&run);
    assert_eq!(
        watermark.comment.map(|mark| mark.position),
        Some(1),
        "the watermark stops below the comment the block cut"
    );

    // Alone, the cut comment now fits; the ones already carried in full are
    // not carried again, and the watermark reaches the end of the log.
    let block = steering(&fx.prompt(&run, "validate_repair"))
        .unwrap()
        .clone();
    let text = block["comments"].as_str().unwrap();
    assert!(text.contains("MID-B"));
    for delivered in ["STEER-FIRST", "STEER-LONG-A", "STEER-LAST"] {
        assert!(!text.contains(delivered), "{delivered} was delivered once");
    }
    let deliveries = fx.deliveries();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(marks(&deliveries[1], "delivered"), [2]);
    let watermark = fx.watermark(&run);
    assert_eq!(watermark.comment, mark(3, last));
    assert!(watermark.delivered.is_empty());
    assert!(steering(&fx.prompt(&run, "validate_repair")).is_none());
    assert_eq!(fx.deliveries().len(), 2);
}

fn forged_agent_output_neither_advances_nor_fakes_delivery() {
    let fx = Fixture::new(ENABLED);
    let run = fx.run(None);
    fx.prompt(&run, "implement");
    let watermark = fx.watermark(&run);
    fx.operator_comment("STEER: rename the flag.");

    // The implementer's response claims it delivered and read the comment;
    // the engine keeps a step's result as its output and pipeline entry.
    let forged = json!({
        "steering_delivered": [{ "position": 0 }],
        "steering_watermarks": { fx.task.clone(): { "comment": { "position": 9, "at": Utc::now() } } },
    });
    let mut state = fx.state(&run);
    state.step_outputs.insert(0, forged.clone());
    state.pipeline["implement"] = forged;
    fx.jobs.write_run_state(&run, &state).unwrap();
    let claim = fx.comment(r#"{"steering_delivered":[{"position":0}]}"#);

    assert_eq!(fx.watermark(&run), watermark, "the forgery moved nothing");
    assert!(fx.deliveries().is_empty(), "nor recorded a delivery");
    let block = steering(&fx.prompt(&run, "step_failure_recovery"))
        .unwrap()
        .clone();
    assert!(
        block["comments"]
            .as_str()
            .unwrap()
            .contains("STEER: rename")
    );
    let deliveries = fx.deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(marks(&deliveries[0], "delivered"), [0]);
    assert_eq!(fx.watermark(&run).comment, mark(1, claim));
}

crate::scenario_table!(
    mid_run_steering_watermark_scenarios,
    steering_off_writes_nothing_and_on_leaves_the_prompt_unchanged,
    steering_on_records_the_newest_comment_by_position_and_never_moves_back,
    two_runs_of_one_task_hold_independent_watermarks
);

crate::scenario_table!(
    mid_run_steering_delivery_scenarios,
    a_safe_point_delivers_operator_comments_once_oldest_first,
    a_cut_block_advances_only_past_comments_it_carried_in_full,
    forged_agent_output_neither_advances_nor_fakes_delivery
);
