//! [ORB-10801] `orbit run job` / `orbit job run` submission behaviour.
//!
//! A job run is submitted to a detached worker and the caller returns as soon
//! as the run is durable. These cover the four submission outcomes the CLI
//! distinguishes — accepted, queued, worker-startup failure, and a run that
//! fails asynchronously — plus the direct-path definition snapshot that makes
//! asynchronous execution safe for an unmanaged YAML file.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::JobRunState;
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::application::job::JobRunListParams;
use crate::application::job::pipeline::{run_definition_snapshot_path, worker_command_override};

/// A finite worker for submission-path assertions. The startup observer reaps
/// it after the one-second bound, so focused test runs leave no fixture child.
const IDLE_WORKER: &str = "sleep 1";

fn test_runtime() -> (TempDir, OrbitRuntime) {
    let root = TempDir::new().expect("tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

/// A worker program that is not this test binary. Re-execing `current_exe`
/// would hand libtest the worker argv as test filters and recurse.
struct WorkerOverride;

impl WorkerOverride {
    fn shell(script: &str) -> Self {
        worker_command_override::set(["sh", "-c", script]);
        Self
    }

    fn missing_program() -> Self {
        worker_command_override::set(["/nonexistent/orbit-pipeline-worker"]);
        Self
    }
}

impl Drop for WorkerOverride {
    fn drop(&mut self) {
        worker_command_override::clear();
    }
}

fn job_yaml(name: &str, max_active_runs: u32) -> String {
    format!(
        r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  max_active_runs: {max_active_runs}
  steps:
    - id: nap
      spec:
        type: deterministic
        action: sleep
        config: {{}}
"#
    )
}

fn seed_catalog_job(runtime: &OrbitRuntime, name: &str, max_active_runs: u32) {
    let jobs_dir = runtime.paths().jobs_dir.clone();
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(
        jobs_dir.join(format!("{name}.yaml")),
        job_yaml(name, max_active_runs),
    )
    .expect("write catalog job");
}

fn write_job_file(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).expect("create job dir");
    let path = dir.join(format!("{name}.yaml"));
    std::fs::write(&path, contents).expect("write job file");
    path
}

#[test]
fn catalog_submission_persists_a_pending_run_and_returns_before_completion() {
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_ok", 1);
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let invoke = runtime
        .submit_job_run("qa_submit_ok", serde_json::json!({}), Some("test"))
        .expect("submission succeeds");

    assert_eq!(invoke.job_name, "qa_submit_ok");
    assert!(!invoke.queued, "an unclaimed slot must not report queued");
    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_submit_ok".to_string()),
            ..Default::default()
        })
        .expect("list runs");
    assert_eq!(runs.len(), 1, "submission persists exactly one run");
    assert_eq!(runs[0].run_id, invoke.run_id);
    // The submission returns while the run is still owed to its worker: it
    // claims durability and startup, never the eventual job outcome.
    assert_eq!(runs[0].state, JobRunState::Pending);
}

#[test]
fn submission_reports_queued_when_the_job_is_at_its_active_run_limit() {
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_queued", 1);
    runtime
        .stores()
        .jobs()
        .insert_job_run("qa_submit_queued", 1, Utc::now(), None, None)
        .expect("insert the run already holding the slot");
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let invoke = runtime
        .submit_job_run("qa_submit_queued", serde_json::json!({}), Some("test"))
        .expect("a queued submission still succeeds");

    assert!(
        invoke.queued,
        "the second run of a max_active_runs=1 job must report queued"
    );
}

#[test]
fn test_submission_requires_an_explicit_worker_override_before_spawning() {
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_missing_override", 1);

    let error = runtime
        .submit_job_run(
            "qa_submit_missing_override",
            serde_json::json!({}),
            Some("test"),
        )
        .expect_err("test builds must not re-exec the libtest binary as a pipeline worker");
    assert!(
        error
            .to_string()
            .contains("requires an explicit worker command override"),
        "the submission must fail closed before spawning a worker: {error}"
    );

    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_submit_missing_override".to_string()),
            ..Default::default()
        })
        .expect("list submitted runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, JobRunState::Interrupted);
    assert_eq!(runs[0].pid, None, "no worker process may have started");
}

/// A worker that cannot start is the *submission's* failure: the caller is
/// told, and the run it already persisted is terminalized rather than left
/// pending forever.
#[test]
fn worker_startup_failure_fails_the_submission_and_terminalizes_the_run() {
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_no_worker", 1);
    let _worker = WorkerOverride::missing_program();

    let error = runtime
        .submit_job_run("qa_submit_no_worker", serde_json::json!({}), Some("test"))
        .expect_err("a worker that cannot spawn must fail the submission");
    assert!(
        error.to_string().contains("spawn pipeline worker"),
        "the failure must name the worker startup: {error}"
    );

    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_submit_no_worker".to_string()),
            ..Default::default()
        })
        .expect("list runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].state,
        JobRunState::Interrupted,
        "a run whose worker never started must not stay pending"
    );
}

/// A run that dies after submission is reported by the waiter, never by the
/// submission — which had already succeeded.
#[test]
fn waiting_surfaces_a_terminal_state_the_submission_could_not_know() {
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_async_fail", 1);
    let _worker = WorkerOverride::shell("echo 'worker gave up' >&2; exit 23");

    let invoke = runtime
        .submit_job_run("qa_submit_async_fail", serde_json::json!({}), Some("test"))
        .expect("submission succeeds even though the run will not");

    let wait = runtime
        .wait_pipeline_runs(std::slice::from_ref(&invoke.run_id), 30, 1, Some("test"))
        .expect("wait completes");
    let entry = wait
        .results
        .into_iter()
        .find(|entry| entry.run_id == invoke.run_id)
        .expect("wait reports the submitted run");

    assert_eq!(entry.status, "interrupted");
    let detail = entry.error.expect("a failed wait must carry a diagnostic");
    assert!(
        detail.contains("worker log"),
        "the diagnostic must point at the worker log: {detail}"
    );
}

/// A direct path names an unmanaged file. The submitted run must execute the
/// definition that was validated, so the exact YAML is pinned next to the run
/// before submission returns — later edits and deletions cannot reach it.
#[test]
fn direct_path_submission_pins_the_validated_definition_against_later_edits() {
    let (root, runtime) = test_runtime();
    let source = write_job_file(
        &root.path().join("loose"),
        "qa_direct",
        &job_yaml("qa_direct", 1),
    );
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let invoke = runtime
        .submit_job_run(
            &source.display().to_string(),
            serde_json::json!({}),
            Some("test"),
        )
        .expect("direct-path submission succeeds");

    let snapshot = run_definition_snapshot_path(&runtime.paths().job_runs_dir, &invoke.run_id);
    let pinned = std::fs::read_to_string(&snapshot).expect("definition snapshot is durable");
    assert_eq!(pinned, job_yaml("qa_direct", 1));

    // Mutate, then delete, the file the operator named.
    std::fs::write(&source, job_yaml("qa_direct_rewritten", 9)).expect("rewrite source");
    std::fs::remove_file(&source).expect("delete source");

    let run = runtime.show_job_run(&invoke.run_id).expect("run persisted");
    let (resolved_path, spec) = runtime
        .resolve_run_definition(&run)
        .expect("the worker still resolves a definition");
    assert_eq!(resolved_path, snapshot);
    assert_eq!(
        spec.max_active_runs, 1,
        "the pinned definition is unchanged"
    );
    assert_eq!(spec.steps.len(), 1);
    assert_eq!(spec.steps[0].id, "nap");
}

/// Direct-path validation runs in the submitting process, so a definition the
/// worker could not finish is refused before any run exists to inspect.
#[test]
fn direct_path_submission_refuses_a_retired_declaration_before_persisting_a_run() {
    let (root, runtime) = test_runtime();
    let source = write_job_file(
        &root.path().join("loose"),
        "qa_direct_retired",
        r#"schemaVersion: 2
kind: Job
metadata:
  name: qa_direct_retired
spec:
  state: enabled
  kind: workflow
  steps:
    - id: assess
      spec:
        type: agent_loop
        instruction: assess
        tools: []
      session: assessor
"#,
    );
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let error = runtime
        .submit_job_run(
            &source.display().to_string(),
            serde_json::json!({}),
            Some("test"),
        )
        .expect_err("a retired `session:` binding must be refused");
    assert!(
        matches!(error, OrbitError::InvalidInput(ref message) if message.contains("CLI agent path")),
        "the refusal must carry the migration: {error:?}"
    );
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .expect("list runs")
            .is_empty(),
        "a refused submission persists no run"
    );
}

/// Both spellings resolve through the same core entry point, so a subroutine
/// is refused identically whichever one an operator typed.
#[test]
fn submission_refuses_a_subroutine_job() {
    let (_root, runtime) = test_runtime();
    let jobs_dir = runtime.paths().jobs_dir.clone();
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(
        jobs_dir.join("qa_subroutine.yaml"),
        job_yaml("qa_subroutine", 1).replace("kind: workflow", "kind: subroutine"),
    )
    .expect("write subroutine job");
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let error = runtime
        .submit_job_run("qa_subroutine", serde_json::json!({}), Some("test"))
        .expect_err("a subroutine cannot be run directly");
    assert!(
        error.to_string().contains("kind: subroutine"),
        "the refusal must name the declared kind: {error}"
    );
}

/// Guard against the submission quietly waiting: the caller returns while the
/// worker is still alive.
#[test]
fn submission_returns_while_its_worker_is_still_running() {
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_nonblocking", 1);
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let started = std::time::Instant::now();
    runtime
        .submit_job_run("qa_submit_nonblocking", serde_json::json!({}), Some("test"))
        .expect("submission succeeds");

    assert!(
        started.elapsed() < Duration::from_secs(4),
        "submission must not block on the worker it started"
    );
}

#[test]
fn auto_complexity_pool_is_captured_at_submission_and_retained_by_real_resume() {
    use crate::application::job::pipeline::{ChildPipelineAdmission, ChildSubmission};
    use crate::application::task::TaskAddParams;
    use orbit_config::ComplexityCrewPools;
    use orbit_types::task::TaskComplexity;
    use serde_json::json;

    let (_root, runtime) = test_runtime();
    for job in [
        "workspace_auto_pipeline",
        "task_auto_pipeline",
        "task_gate_pipeline",
    ] {
        write_job_file(
            &runtime.paths().global_dir.join("resources/jobs"),
            job,
            &job_yaml(job, 10),
        );
    }
    let _worker = WorkerOverride::shell("sleep 5");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Automatic crew persistence".into(),
            description: "Admission and resume preserve crew evidence".into(),
            plan: "Validate persisted input".into(),
            complexity: TaskComplexity::Medium,
            status: Some(orbit_types::task::TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("task");
    let parent = runtime
        .submit_workspace_auto_run(
            None,
            None,
            crate::CompletionPolicy::Review,
            &[],
            &ComplexityCrewPools {
                medium: Some(vec!["grok".into(), "terra".into()]),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("submit coordinator");
    let parent_run = runtime
        .get_job_run_backend(&parent.run_id)
        .expect("read coordinator")
        .expect("coordinator");
    assert_eq!(
        parent_run.input.as_ref().expect("input")["auto_crew_pools"]["medium"]["source"],
        "run_input.medium_complexity_crews"
    );
    let child = runtime
        .submit_child_pipeline_run(
            "task_auto_pipeline",
            json!({"task_ids": [task.id]}),
            None,
            None,
            &ChildPipelineAdmission {
                parent_run_id: parent.run_id,
                parent_step_id: Some("ship_leaves".into()),
                action: "invoke_detached".into(),
                blocking: false,
            },
        )
        .expect("admit child");
    let ChildSubmission::Submitted(child) = child else {
        panic!("child admitted");
    };
    let run = runtime
        .get_job_run_backend(&child.run_id)
        .expect("read child")
        .expect("child");
    let input = run.input.expect("child input");
    assert!(["grok", "terra"].contains(&input["crew"].as_str().expect("crew")));
    assert_eq!(
        input["crew_selection"]["source"],
        "run_input.medium_complexity_crews"
    );
    assert!(input.get("allowed_crews").is_none());
    let nested = runtime
        .submit_child_pipeline_run(
            "task_gate_pipeline",
            json!({"task_ids": [task.id]}),
            None,
            None,
            &ChildPipelineAdmission {
                parent_run_id: child.run_id.clone(),
                parent_step_id: Some("gate".into()),
                action: "invoke_and_wait".into(),
                blocking: true,
            },
        )
        .expect("admit same-task pipeline");
    let ChildSubmission::Submitted(nested) = nested else {
        panic!("nested admitted");
    };
    let nested_input = runtime
        .get_job_run_backend(&nested.run_id)
        .expect("read nested")
        .expect("nested")
        .input
        .expect("input");
    assert_eq!(nested_input["crew_selection"], input["crew_selection"]);
    for run_id in [&nested.run_id, &child.run_id] {
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(run_id, Utc::now(), std::process::id())
            .expect("start fixture before recording failure");
        runtime
            .finalize_job_run_with_reservation_cleanup(
                run_id,
                JobRunState::Failed,
                Utc::now(),
                Some(1),
                orbit_store::contracts::TaskReservationReleaseReason::RunTerminal,
            )
            .expect("finish fixture run");
    }
    let resumed = runtime
        .submit_resume_run(&child.run_id, None, None)
        .expect("resume child");
    let resumed_input = runtime
        .get_job_run_backend(&resumed.run_id)
        .expect("read resume")
        .expect("resumed")
        .input
        .expect("input");
    assert_eq!(resumed_input["crew"], input["crew"]);
    assert_eq!(resumed_input["crew_selection"], input["crew_selection"]);
}
