//! `invoke_and_wait` hands its parent a bounded child summary, so a finished
//! fan-out parent stores no copy of any child's pipeline [ORB-14587].
//!
//! A gate's wait that expires on its unfinished delivery child cancels it,
//! keeping its candidate, unless the child settled after the wait's last look;
//! the run graph never ends with terminal parents and a live child
//! [ORB-15643]. The wait runs on an injected clock, so its 4 h deadline
//! passes at once and the child can be settled between two of its polls.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{TaskComplexity, TaskStatus, TaskType};
use orbit_types::workflow::{ChildDispatchPhase, JobRunState, PipelineState};
use serde_json::{Value, json};

use super::super::invoke::invoke_and_wait_with;
use super::super::pipeline_success_guard;
use crate::OrbitRuntime;
use crate::application::job::DEFAULT_JOB_FILES;
use crate::application::job::pipeline::PipelineWaitClock;
use crate::application::task::TaskAddParams;
use crate::application::test_support::runtime_with_workspace_config;

/// A finished four-pilot `ci_failure_sweep_pipeline` run in the encoding
/// before compaction: each pilot's wait entry carries the child's whole
/// pipeline and is held three times (`pilots`, its `pilot_results` alias and
/// `step_outputs`), and the CI evidence twice. Recorded from a live sweep with
/// every content string replaced by filler; structure and duplication are the
/// recorded run's.
const RECORDED_SWEEP: &str = include_str!("fixtures/ci_sweep_four_pilots_state.json");

#[test]
fn a_finished_ci_sweep_stores_child_summaries_and_no_resume_state() {
    let (_root, runtime, _) = runtime_with_workspace_config(None);
    let jobs = runtime.stores().jobs();
    let parent = jobs
        .insert_job_run("ci_failure_sweep_pipeline", 1, Utc::now(), None, None)
        .expect("insert sweep");
    jobs.mark_job_run_running(&parent.run_id, Utc::now(), std::process::id())
        .expect("start sweep");
    let mut recorded: PipelineState =
        serde_json::from_str(RECORDED_SWEEP).expect("recorded sweep state");
    recorded.run_id = parent.run_id.clone();
    let recorded_bytes = serde_json::to_string(&recorded)
        .expect("encode recorded state")
        .len();
    runtime
        .write_run_state(&parent.run_id, &recorded)
        .expect("seed sweep state");
    let entries = recorded.pipeline["pilots"]
        .as_array()
        .expect("recorded pilot entries")
        .clone();
    assert_eq!(entries.len(), 4);

    // Each pilot, as `invoke_and_wait` now returns its child's wait entry.
    let summaries = entries
        .iter()
        .map(|entry| {
            assert!(entry["pipeline"].is_object(), "recorded entry: {entry}");
            invoke_and_wait_with(
                &runtime,
                "invoke_and_wait",
                &json!({
                    "job_name": "task_pilot_pipeline",
                    "run_input": {},
                    "run_id": parent.run_id,
                    "step_id": "pilot",
                }),
                |_| Ok(json!({"run_id": entry["run_id"], "queued": false})),
                |_| Ok(json!({"results": [entry]})),
            )
            .expect("pilot wait")
        })
        .collect::<Vec<_>>();
    for (summary, entry) in summaries.iter().zip(&entries) {
        assert_eq!(
            summary,
            &json!({
                "run_id": entry["run_id"],
                "status": entry["status"],
                "finished_at": entry["finished_at"],
            }),
            "the summary keeps what guards and readers use, never the child pipeline"
        );
    }
    let guard = pipeline_success_guard(
        &runtime,
        "pipeline_success_guard",
        &json!({"context": "ci-failure sweep pilot child", "results": summaries}),
    )
    .expect("the sweep's guard passes on summaries");
    assert_eq!(guard["checked_count"], 4);

    // The fan-in checkpoint and the terminal write, as the engine makes them.
    let collected = Value::Array(summaries);
    runtime
        .checkpoint_step(
            &parent.run_id,
            2,
            "pilots",
            &collected,
            &BTreeMap::from([("pilot_results".to_string(), collected.clone())]),
        )
        .expect("fan-in checkpoint");
    jobs.finalize_job_run(&parent.run_id, JobRunState::Success, Utc::now(), Some(0))
        .expect("finish sweep");

    let stored_bytes = runtime
        .sqlite_store()
        .expect("store")
        .connection()
        .lock()
        .expect("store connection")
        .query_row(
            "SELECT length(pipeline_state_json) FROM job_run_states WHERE run_id = ?1",
            [&parent.run_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("stored state length");
    let stored_bytes = usize::try_from(stored_bytes).expect("length");
    assert!(
        stored_bytes * 10 <= recorded_bytes * 4,
        "a finished four-pilot sweep must store at least 60% less state than the recorded \
         encoding: {stored_bytes} of {recorded_bytes} bytes"
    );

    let state = runtime
        .read_run_state(&parent.run_id)
        .expect("read sweep")
        .expect("sweep state");
    assert!(state.step_outputs.is_empty() && state.compound_outputs.is_empty());
    assert_eq!(state.step_output(2), Some(&collected));
    assert_eq!(state.pipeline["pilot_results"], collected);
    assert_eq!(
        state.step_output(0),
        Some(&recorded.pipeline["collect"]),
        "outputs still held in the pipeline stay readable by step"
    );
    let lineage = |state: &PipelineState| {
        state
            .child_dispatches
            .iter()
            .map(|dispatch| (dispatch.child_run_id.clone(), dispatch.child_status.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lineage(&state),
        lineage(&recorded),
        "child lineage survives"
    );
}

/// The gate's dispatch timeout and poll interval, as `task_gate_pipeline`
/// passes them.
const GATE_TIMEOUT_SECONDS: u64 = 14_400;
const GATE_POLL_SECONDS: u64 = 3_600;

/// A wait clock whose sleeps pass instantly, running `on_sleep` first.
struct FakeClock<'a> {
    elapsed: Cell<Duration>,
    on_sleep: &'a dyn Fn(Duration),
}

impl PipelineWaitClock for FakeClock<'_> {
    fn elapsed(&self) -> Duration {
        self.elapsed.get()
    }

    fn sleep(&self, duration: Duration, _exits_seen: u64) {
        (self.on_sleep)(self.elapsed.get());
        self.elapsed.set(self.elapsed.get() + duration);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=orbit@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A running parent gate and a running child, both owned by this process, so
/// cancelling the child signals nothing.
fn running_run(runtime: &OrbitRuntime, job: &str, input: Option<Value>) -> String {
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(job, 1, Utc::now(), input, None)
        .expect("insert run");
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), job.to_string(), json!({})),
        )
        .expect("seed run state");
    run.run_id
}

/// Run the gate's `dispatch_child` against `child`, its wait expiring at the
/// gate's dispatch timeout unless `on_sleep` settles the child first.
fn dispatch_child(
    runtime: &OrbitRuntime,
    parent: &str,
    child: &str,
    on_timeout: Option<&str>,
    on_sleep: &dyn Fn(Duration),
) -> (Value, Duration) {
    let mut input = json!({
        "job_name": "task_pr_pipeline",
        "run_input": {},
        "run_id": parent,
        "step_id": "dispatch_child",
        "timeout_seconds": GATE_TIMEOUT_SECONDS,
        "poll_interval_seconds": GATE_POLL_SECONDS,
    });
    if let Some(on_timeout) = on_timeout {
        input["on_timeout"] = json!(on_timeout);
    }
    let clock = FakeClock {
        elapsed: Cell::new(Duration::ZERO),
        on_sleep,
    };
    let summary = invoke_and_wait_with(
        runtime,
        "invoke_and_wait",
        &input,
        |_| Ok(json!({"run_id": child, "queued": false})),
        |_| {
            let result = runtime.wait_pipeline_runs_with_clock(
                &[child.to_string()],
                GATE_TIMEOUT_SECONDS,
                GATE_POLL_SECONDS,
                None,
                &clock,
            )?;
            Ok(serde_json::to_value(result).expect("encode wait result"))
        },
    )
    .expect("dispatch_child");
    (summary, clock.elapsed.get())
}

/// The parent's dispatch record of `child`: its phase and the status it closed on.
fn dispatch_record(
    runtime: &OrbitRuntime,
    parent: &str,
    child: &str,
) -> (ChildDispatchPhase, Option<String>) {
    let state = runtime
        .read_run_state(parent)
        .expect("read parent")
        .expect("parent state");
    let dispatch = state
        .child_dispatches
        .iter()
        .find(|dispatch| dispatch.child_run_id == child)
        .expect("child dispatch record");
    (dispatch.phase, dispatch.child_status.clone())
}

fn run_state(runtime: &OrbitRuntime, run_id: &str) -> JobRunState {
    runtime
        .get_job_run_backend(run_id)
        .expect("read run")
        .expect("run exists")
        .state
}

#[test]
fn an_expired_gate_wait_cancels_its_delivery_child_and_keeps_its_candidate() {
    let (root, runtime, repo) = runtime_with_workspace_config(None);
    // The installed catalog the cancelled child's failure handoff comes from.
    let jobs_dir = root.path().join("home/.orbit/resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("jobs dir");
    let (_, delivery) = DEFAULT_JOB_FILES
        .iter()
        .find(|(name, _)| *name == "task_pr_pipeline")
        .expect("shipped task_pr_pipeline");
    std::fs::write(jobs_dir.join("task_pr_pipeline.yaml"), delivery).expect("install job");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("write ignore");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Deliver past the gate deadline".into(),
            description: "A delivery still validating when its gate stops waiting.".into(),
            acceptance_criteria: vec!["The candidate survives the timeout.".into()],
            plan: "1. Implement.".into(),
            complexity: TaskComplexity::Low,
            task_type: Some(TaskType::Bug),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add task")
        .id;
    let worktree = root.path().join("worktree");
    let branch = format!("orbit/{task}");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            worktree.to_str().expect("utf-8 path"),
        ],
    );

    let parent = running_run(&runtime, "task_gate_pipeline", None);
    let child = running_run(
        &runtime,
        "task_pr_pipeline",
        Some(json!({"task_ids": [task]})),
    );
    runtime
        .apply_task_automation_update(
            &task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(child.clone()),
                ..Default::default()
            },
        )
        .expect("bind task to the child");
    // The child got as far as `validate`: everything before it recorded an
    // output, and the implementation is still uncommitted in its worktree.
    runtime
        .stores()
        .jobs()
        .update_run_state(&child, &mut |_, state| {
            state.pipeline = json!({
                "worktree": {
                    "workspace_path": worktree,
                    "job_run_id": child,
                    "branch": branch,
                },
                "review_preflight": {"skipped": true},
                "resume_candidate": {"resumed": false},
                "implement_bundle": [{"status": "success"}],
                "commit": {"committed": false},
                "prepare_branch": {"branch": branch},
                "sync_base": {"synced": true},
            });
            Ok(())
        })
        .expect("seed child progress");
    std::fs::write(worktree.join("fix.txt"), "the implementation\n").expect("dirty edit");

    let (summary, waited) =
        dispatch_child(&runtime, &parent, &child, Some("cancel_child"), &|_| {});

    assert_eq!(
        waited,
        Duration::from_secs(GATE_TIMEOUT_SECONDS),
        "the deadline is a hard ceiling: the wait is never extended"
    );
    assert_eq!(summary["status"], "timeout", "summary: {summary}");
    let error = summary["error"].as_str().expect("error");
    assert!(error.starts_with("child_timeout: "), "error: {error}");
    assert_eq!(summary["child_cancelled"], true, "summary: {summary}");
    assert!(
        summary.get("child_live").is_none(),
        "a cancelled child is not live: {summary}"
    );
    assert_eq!(run_state(&runtime, &child), JobRunState::Cancelled);
    assert_eq!(
        dispatch_record(&runtime, &parent, &child),
        (ChildDispatchPhase::Terminal, Some("timeout".to_string()))
    );

    // The candidate is committed on the child's branch and recorded for the
    // task's next run to resume.
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    assert_eq!(
        git(&worktree, &["status", "--porcelain"]),
        "",
        "summary: {summary}"
    );
    assert_eq!(
        git(&worktree, &["show", "HEAD:fix.txt"]),
        "the implementation"
    );
    assert_eq!(summary["candidate"]["head_sha"], head.as_str());
    assert_eq!(summary["candidate"]["decision"], "held_child_timeout");
    assert!(
        error.contains(&head),
        "the error names the candidate: {error}"
    );
    let checkpoint = runtime
        .read_run_state(&child)
        .expect("read child")
        .expect("child state")
        .failure_activity_checkpoint
        .expect("the handoff is recorded for candidate_resume");
    assert_eq!(checkpoint.activity_name, "pr_failure_handoff");
    assert_eq!(checkpoint.failed_step_id, "validate");
    assert_eq!(checkpoint.output["decision"], "held_child_timeout");
    assert_eq!(checkpoint.output["pr_created"], false);

    // The task goes back to the backlog naming the timeout and the candidate.
    let settled = runtime.get_task(&task).expect("task");
    assert_eq!(settled.status, TaskStatus::Backlog);
    let note = runtime
        .get_task_history(&task)
        .expect("history")
        .into_iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Backlog))
        .and_then(|entry| entry.note)
        .expect("requeue note");
    assert!(
        note.contains("child_timeout") && note.contains(&head),
        "the requeue names the timeout and the kept candidate: {note}"
    );
}

#[test]
fn a_child_that_finishes_as_the_wait_expires_reports_its_own_outcome() {
    let (_root, runtime, _) = runtime_with_workspace_config(None);
    let parent = running_run(&runtime, "task_gate_pipeline", None);
    let child = running_run(&runtime, "task_pr_pipeline", None);
    // The child settles on the last poll interval, after the wait's last
    // look at it and before the parent acts on the deadline.
    let on_sleep = |elapsed: Duration| {
        if elapsed + Duration::from_secs(GATE_POLL_SECONDS)
            >= Duration::from_secs(GATE_TIMEOUT_SECONDS)
        {
            runtime
                .stores()
                .jobs()
                .finalize_job_run(&child, JobRunState::Success, Utc::now(), Some(1))
                .expect("finish child");
        }
    };

    let (summary, _) = dispatch_child(&runtime, &parent, &child, Some("cancel_child"), &on_sleep);

    assert_eq!(summary["status"], "success", "summary: {summary}");
    assert!(
        summary.get("child_cancelled").is_none() && summary.get("child_live").is_none(),
        "a finished child is neither cancelled nor live: {summary}"
    );
    assert_eq!(run_state(&runtime, &child), JobRunState::Success);
    assert_eq!(
        dispatch_record(&runtime, &parent, &child),
        (ChildDispatchPhase::Terminal, Some("success".to_string()))
    );
}
