//! Shared test helpers reused by the api submodules.

use axum::body::to_bytes;
use axum::response::Response;
use chrono::Utc;
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
use serde_json::Value;

/// First word the substitute pipeline worker writes to its run's worker log.
pub(super) const SUBSTITUTE_WORKER_MARKER: &str = "substitute-pipeline-worker";

/// Launch a shell stub, not this test binary, as every pipeline worker this
/// process spawns [ORB-12902]. Call it in any test whose request can submit a
/// run (ship, resume, auto): the production spawn refuses to re-exec a libtest
/// harness, so an unsubstituted submission fails. The stub logs
/// `SUBSTITUTE_WORKER_MARKER <run_id>` and exits without claiming the run.
pub(super) fn substitute_pipeline_worker() {
    orbit_core::test_support::install_substitute_pipeline_worker([
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "echo {SUBSTITUTE_WORKER_MARKER} {}",
            orbit_core::test_support::RUN_ID_PLACEHOLDER
        ),
    ]);
}

pub(super) fn write_lines(path: &std::path::Path, lines: &[String]) {
    let mut content = String::new();
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    std::fs::write(path, content).expect("write fixture");
}

pub(super) fn write_replay_job(runtime: &OrbitRuntime, name: &str) -> std::path::PathBuf {
    write_replay_job_under(&runtime.global_root(), name)
}

/// Writes the stub sleep-workflow job asset into `<root>/resources/jobs`.
/// Default-named jobs (e.g. `task_auto_pipeline`) are loaded from the *global*
/// orbit root, so global-mode fixtures must seed them there rather than in a
/// workspace's `.orbit` directory.
pub(super) fn write_replay_job_under(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let jobs_dir = root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    let path = jobs_dir.join(format!("{name}.yaml"));
    std::fs::write(
        &path,
        format!(
            r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  steps:
    - id: nap
      spec:
        type: deterministic
        action: sleep
        config: {{}}
"#
        ),
    )
    .expect("write replay job");
    path
}

pub(super) fn seed_run(
    runtime: &OrbitRuntime,
    run_id: &str,
    job_id: &str,
    state: JobRunState,
) -> JobRun {
    let now = Utc::now();
    let run = JobRun {
        executed_on: None,
        run_id: run_id.to_string(),
        job_id: job_id.to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: matches!(
            state,
            JobRunState::Running
                | JobRunState::Success
                | JobRunState::Failed
                | JobRunState::Timeout
                | JobRunState::Cancelled
        )
        .then_some(now),
        finished_at: state.is_terminal().then_some(now),
        duration_ms: state.is_terminal().then_some(0),
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    write_seeded_run(runtime, &run);
    run
}

pub(super) fn write_seeded_run(runtime: &OrbitRuntime, run: &JobRun) {
    let workspace_id = runtime.workspace_id().expect("workspace id");
    runtime
        .sqlite_store()
        .expect("sqlite store")
        .upsert_job_run_for_workspace(&workspace_id, run, None)
        .expect("insert job run");
}

pub(super) async fn body_json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    serde_json::from_slice(&bytes).expect("json response")
}
