use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::maintenance::migration::SUPPORTED_SCHEMA_VERSION;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRunStartOutcome, JobRunState};
use orbit_types::workspace::WorkspacePaths;
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::application::job::JobRunListParams;
use crate::application::job::pipeline::{
    configure_pipeline_worker_command, configure_pipeline_worker_stdio, pipeline_worker_log_path,
    pipeline_worker_profile_file, pipeline_worker_root_override,
    resolve_pipeline_worker_executable, run_definition_snapshot_path, worker_command_override,
    worker_observer_read_counter,
};
use crate::application::task::TaskAddParams;
use crate::application::workflow::{CompletionPolicy, ShipMode};

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

fn test_runtime_with_named_crews() -> (TempDir, OrbitRuntime) {
    let root = TempDir::new().expect("tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        r#"
[workflow]
default_crew = "primary"

[crews.primary]
provider = "codex"
backend = "cli"
model = "default-model"

[crews.terra]
provider = "codex"
backend = "cli"
model = "terra-model"

[crews.sol]
provider = "codex"
backend = "cli"
model = "sol-model"
"#,
    )
    .expect("write crew config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

struct WorkerOverride;

impl WorkerOverride {
    fn shell(script: &str) -> Self {
        worker_command_override::set(["sh", "-c", script]);
        Self
    }
}

impl Drop for WorkerOverride {
    fn drop(&mut self) {
        worker_command_override::clear();
    }
}

fn add_backlog_task(runtime: &OrbitRuntime) -> String {
    runtime
        .add_task(TaskAddParams {
            title: "Ship submission fixture".to_string(),
            description: "A task selected by a ship-submission test.".to_string(),
            ..Default::default()
        })
        .expect("create backlog task")
        .id
}

#[test]
fn pipeline_worker_command_discovers_registered_workspace_from_cwd() {
    let workspace = Path::new("/registered/workspace");
    let mut command = Command::new("orbit");

    configure_pipeline_worker_command(&mut command, workspace, "jrun-child", None);

    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec![
            OsStr::new("job"),
            OsStr::new("run-pipeline-worker"),
            OsStr::new("jrun-child"),
        ],
        "an unpinned parent must not pass --root; that pins both roots and disconnects the worker from the global store"
    );
    assert_eq!(command.get_current_dir(), Some(workspace));
}

#[test]
fn pipeline_worker_command_forwards_explicit_root_to_the_detached_worker() {
    let workspace = Path::new("/registered/workspace");
    let pinned_root = Path::new("/tmp/custom-orbit");
    let mut command = Command::new("orbit");

    configure_pipeline_worker_command(&mut command, workspace, "jrun-child", Some(pinned_root));

    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec![
            OsStr::new("--root"),
            OsStr::new("/tmp/custom-orbit"),
            OsStr::new("job"),
            OsStr::new("run-pipeline-worker"),
            OsStr::new("jrun-child"),
        ],
        "a parent constructed with --root must forward that same global store to the worker"
    );
    assert_eq!(command.get_current_dir(), Some(workspace));
    assert!(
        command
            .get_envs()
            .any(|(key, value)| key == OsStr::new("ORBIT_ROOT") && value.is_none()),
        "an explicit --root must not let inherited ORBIT_ROOT re-select $HOME/.orbit"
    );
}

#[test]
fn pipeline_worker_profile_file_is_none_without_inherited_coverage_env() {
    assert_eq!(
        pipeline_worker_profile_file(Path::new("/tmp/logs"), "jrun-child", None)
            .expect("profile path validation"),
        None
    );
    assert_eq!(
        pipeline_worker_profile_file(Path::new("/tmp/logs"), "jrun-child", Some(OsStr::new("")))
            .expect("profile path validation"),
        None
    );
}

#[test]
fn pipeline_worker_profile_file_rewrites_inherited_coverage_dump_under_the_worker_log_dir() {
    assert_eq!(
        pipeline_worker_profile_file(
            Path::new("/tmp/logs"),
            "jrun-child",
            Some(OsStr::new("target/llvm-cov-target/orbit-%p-%m.profraw")),
        )
        .expect("profile path validation"),
        Some(PathBuf::from("/tmp/logs/jrun-child.%p.profraw"))
    );
}

#[test]
fn pipeline_worker_paths_reject_run_id_path_syntax() {
    for run_id in ["../outside", r"nested\outside", ".", ""] {
        assert!(
            pipeline_worker_log_path(Path::new("/tmp/logs"), run_id).is_err(),
            "run ID {run_id:?} must not become a worker-log path"
        );
        assert!(
            pipeline_worker_profile_file(
                Path::new("/tmp/logs"),
                run_id,
                Some(OsStr::new("coverage.profraw")),
            )
            .is_err(),
            "run ID {run_id:?} must not become a profile path"
        );
        assert!(
            run_definition_snapshot_path(Path::new("/tmp/job-runs"), run_id).is_err(),
            "run ID {run_id:?} must not become a snapshot path"
        );
    }
}

#[test]
fn pipeline_worker_paths_preserve_safe_run_id_stems() {
    assert_eq!(
        pipeline_worker_log_path(Path::new("/tmp/logs"), "jrun-child.1")
            .expect("worker log path validation"),
        PathBuf::from("/tmp/logs/jrun-child.1.worker.log")
    );
}

#[test]
fn configure_pipeline_worker_stdio_creates_missing_log_directory_after_validation() {
    let root = TempDir::new().expect("tempdir");
    let logs_dir = root.path().join("missing-logs");
    let mut command = Command::new("true");

    let worker_log = configure_pipeline_worker_stdio(&mut command, &logs_dir, "jrun-child")
        .expect("missing log directory is created after validation");

    let resolved_logs = logs_dir
        .canonicalize()
        .expect("canonicalize created log directory");
    assert!(resolved_logs.is_dir());
    assert_eq!(
        worker_log.path(),
        resolved_logs.join("jrun-child.worker.log")
    );
}

#[cfg(unix)]
#[test]
fn configure_pipeline_worker_stdio_rejects_symlinked_log_directory() {
    let root = TempDir::new().expect("tempdir");
    let outside = root.path().join("outside");
    let logs_dir = root.path().join("logs");
    std::fs::create_dir(&outside).expect("create outside directory");
    std::os::unix::fs::symlink(&outside, &logs_dir).expect("create log-directory symlink");
    let mut command = Command::new("true");

    let result = configure_pipeline_worker_stdio(&mut command, &logs_dir, "jrun-child");

    let error = match result {
        Ok(_) => panic!("symlinked log directories must fail closed"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("must not be a symlink"),
        "{error}"
    );
    assert!(
        !outside.join("jrun-child.worker.log").exists(),
        "worker setup must not follow a log-directory symlink"
    );
}

#[cfg(unix)]
#[test]
fn configure_pipeline_worker_stdio_accepts_symlinked_ancestor_of_log_directory() {
    let root = TempDir::new().expect("tempdir");
    let real = root.path().join("real");
    std::fs::create_dir_all(real.join("state")).expect("create real state directory");
    let linked = root.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).expect("create ancestor symlink");

    let logs_dir = linked.join("state").join("logs");
    let mut command = Command::new("true");
    let worker_log = configure_pipeline_worker_stdio(&mut command, &logs_dir, "jrun-child")
        .expect("symlinked ancestors of a real log directory must be accepted");

    let resolved_logs = real
        .join("state")
        .join("logs")
        .canonicalize()
        .expect("canonicalize resolved log directory");
    assert!(resolved_logs.is_dir());
    assert_eq!(
        worker_log.path(),
        resolved_logs.join("jrun-child.worker.log")
    );
    assert!(worker_log.path().is_file());
}

#[test]
fn configure_pipeline_worker_stdio_creates_missing_intermediate_log_directories() {
    let root = TempDir::new().expect("tempdir");
    let logs_dir = root.path().join("state").join("logs");
    let mut command = Command::new("true");

    let worker_log = configure_pipeline_worker_stdio(&mut command, &logs_dir, "jrun-child")
        .expect("missing intermediate directories are created");

    let resolved_logs = logs_dir
        .canonicalize()
        .expect("canonicalize created log directory");
    assert!(resolved_logs.is_dir());
    assert_eq!(
        worker_log.path(),
        resolved_logs.join("jrun-child.worker.log")
    );
}

#[test]
fn configure_pipeline_worker_stdio_rejects_traversal_in_log_directory() {
    let root = TempDir::new().expect("tempdir");
    let logs_dir = root.path().join("nested").join("..").join("logs");
    let mut command = Command::new("true");

    let error = match configure_pipeline_worker_stdio(&mut command, &logs_dir, "jrun-child") {
        Ok(_) => panic!("traversal components must fail closed"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("must not contain traversal components"),
        "{error}"
    );
    assert!(
        !root
            .path()
            .join("logs")
            .join("jrun-child.worker.log")
            .exists(),
        "worker setup must not resolve traversal into a log directory"
    );
}

#[test]
fn pipeline_worker_root_override_is_none_in_the_default_split_root_layout() {
    let paths = WorkspacePaths::new(
        PathBuf::from("/repo"),
        PathBuf::from("/repo/.orbit"),
        PathBuf::from("/home/user/.orbit"),
    );
    assert_eq!(pipeline_worker_root_override(&paths), None);
}

#[test]
fn pipeline_worker_root_override_forwards_a_pinned_global_store() {
    let paths = WorkspacePaths::new(
        PathBuf::from("/repo"),
        PathBuf::from("/tmp/custom-orbit"),
        PathBuf::from("/tmp/custom-orbit"),
    );
    assert_eq!(
        pipeline_worker_root_override(&paths),
        Some(Path::new("/tmp/custom-orbit"))
    );
}

#[cfg(unix)]
#[test]
fn worker_exit_before_claim_terminalizes_persisted_run_with_diagnostic() {
    let (_root, runtime) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_gate_pipeline", 1, Utc::now(), None, None)
        .expect("insert pending run");
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "printf 'worker stdout context\\n'; \
         printf 'action registration missing: routine_dispatch\\n' >&2; \
         exit 23",
    ]);
    let worker_log =
        configure_pipeline_worker_stdio(&mut command, &runtime.paths().logs_dir, &run.run_id)
            .expect("configure worker log");
    let log_path = worker_log.path().to_owned();

    runtime
        .spawn_pipeline_worker_process(&run.run_id, Some("test"), command, worker_log)
        .expect("spawn detached failing worker fixture");

    let stored = wait_for_worker_ownership_outcome(&runtime, &run.run_id);
    assert_eq!(stored.state, JobRunState::Interrupted);
    assert!(stored.finished_at.is_some());
    assert!(stored.pid.is_none());
    let diagnostic = stored.steps.last().expect("startup diagnostic step");
    let message = diagnostic
        .error_message
        .as_deref()
        .expect("startup diagnostic message");
    assert!(
        message.contains("before claiming the persisted run"),
        "{message}"
    );
    assert!(message.contains("exit status: 23"), "{message}");
    assert!(message.contains("registered workspace"), "{message}");
    assert!(
        message.contains("action registration missing: routine_dispatch"),
        "{message}"
    );
    assert!(
        message.contains(&log_path.display().to_string()),
        "{message}"
    );

    assert_eq!(
        log_path,
        pipeline_worker_log_path(&runtime.paths().logs_dir, &run.run_id)
            .expect("worker log path validation")
    );
    let durable_output = std::fs::read_to_string(&log_path).expect("read durable worker log");
    assert!(durable_output.contains("worker stdout context"));
    assert!(durable_output.contains("action registration missing: routine_dispatch"));

    wait_for_pipeline_audit_event(
        &runtime,
        Some(AuditEventStatus::Failure),
        "startup failure audit",
        |audit| {
            audit.tool_name.as_deref() == Some("pipeline.worker.startup")
                && audit.target_id.as_deref() == Some(run.run_id.as_str())
                && audit
                    .error_message
                    .as_deref()
                    .is_some_and(|error| error.contains("before claiming"))
        },
    );
}

#[cfg(unix)]
#[test]
fn routine_style_detached_worker_is_claimed_within_ownership_window() {
    let (_root, runtime) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("auto_task_scheduler_pipeline", 1, Utc::now(), None, None)
        .expect("insert routine-dispatched run");
    let mut command = Command::new("sh");
    command.args(["-c", "printf 'routine worker startup\\n' >&2; sleep 0.25"]);
    let worker_log =
        configure_pipeline_worker_stdio(&mut command, &runtime.paths().logs_dir, &run.run_id)
            .expect("configure routine worker log");
    let log_path = worker_log.path().to_owned();

    let started = Instant::now();
    let worker_pid = runtime
        .spawn_pipeline_worker_process(&run.run_id, Some("routine-sweep"), command, worker_log)
        .expect("spawn detached routine worker fixture");
    runtime
        .stores()
        .jobs()
        .claim_pending_job_run_owner(&run.run_id, worker_pid)
        .expect("claim routine run");

    let stored = wait_for_worker_ownership_outcome(&runtime, &run.run_id);
    assert_eq!(stored.state, JobRunState::Pending);
    assert_eq!(stored.pid, Some(worker_pid));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "detached routine worker exceeded ownership window"
    );

    wait_for_pipeline_audit_event(&runtime, None, "claimed-worker audit", |audit| {
        audit.tool_name.as_deref() == Some("pipeline.worker.claimed")
            && audit.target_id.as_deref() == Some(run.run_id.as_str())
    });

    assert_eq!(
        log_path,
        pipeline_worker_log_path(&runtime.paths().logs_dir, &run.run_id)
            .expect("worker log path validation")
    );
    let durable_output = wait_for_log_contains(&log_path, "routine worker startup");
    assert!(durable_output.contains("routine worker startup"));

    let terminal = wait_for_worker_terminal(&runtime, &run.run_id);
    assert_eq!(terminal.state, JobRunState::Interrupted);
    let message = terminal
        .steps
        .last()
        .and_then(|step| step.error_message.as_deref())
        .expect("claimed exit diagnostic");
    assert!(message.contains("after claiming"), "{message}");
    assert_child_reaped(worker_pid);
}

#[test]
fn observer_read_counts_isolate_identical_run_ids_in_independent_stores() {
    let (_first_root, first) = test_runtime();
    let (_second_root, second) = test_runtime();
    let run = first
        .stores()
        .jobs()
        .insert_job_run("task_gate_pipeline", 1, Utc::now(), None, None)
        .expect("insert fixture run");
    // Deliberately use the same ID for both stores, independent of allocator
    // timing, to reproduce cross-test counter collisions deterministically.
    let first_count = worker_observer_read_counter::track(&first, &run.run_id);
    let second_count = worker_observer_read_counter::track(&second, &run.run_id);

    worker_observer_read_counter::record(&first.clone(), &run.run_id);
    assert_eq!(first_count.reads(), 1, "runtime clones share the counter");
    assert_eq!(second_count.reads(), 0, "another database is isolated");

    worker_observer_read_counter::record(&second, &run.run_id);
    assert_eq!(first_count.reads(), 1);
    assert_eq!(second_count.reads(), 1);
    drop(first_count);
    worker_observer_read_counter::record(&second, &run.run_id);
    assert_eq!(
        second_count.reads(),
        2,
        "dropping another store keeps this counter"
    );
}

/// Once a worker has claimed the run, the startup observer waits for its exit
/// instead of polling the run row for the rest of the worker lifetime.
#[cfg(unix)]
#[test]
fn claimed_sleeping_worker_does_not_keep_polling_the_run_store() {
    let (_root, runtime) = test_runtime();
    let _worker = WorkerOverride::shell("sleep 3");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_gate_pipeline", 1, Utc::now(), None, None)
        .expect("insert pending run");
    let observer_reads = worker_observer_read_counter::track(&runtime, &run.run_id);
    let mut command = worker_command_override::command(&runtime.paths().repo_root, &run.run_id)
        .expect("build sleeping worker command");
    let worker_log =
        configure_pipeline_worker_stdio(&mut command, &runtime.paths().logs_dir, &run.run_id)
            .expect("configure sleeping worker log");

    let worker_pid = runtime
        .spawn_pipeline_worker_process(&run.run_id, Some("test"), command, worker_log)
        .expect("spawn detached sleeping worker");
    runtime
        .stores()
        .jobs()
        .claim_pending_job_run_owner(&run.run_id, worker_pid)
        .expect("claim sleeping worker");

    wait_for_pipeline_audit_event(&runtime, None, "claimed-worker audit", |audit| {
        audit.tool_name.as_deref() == Some("pipeline.worker.claimed")
            && audit.target_id.as_deref() == Some(run.run_id.as_str())
    });
    let reads_after_claim = observer_reads.reads();

    // The worker remains alive for three seconds. Its terminal diagnostic
    // proves the observer reaped it, while this ownership interval retains the
    // old 25ms poll long enough to make a bounded-read regression observable.
    thread::sleep(Duration::from_millis(250));
    assert!(
        observer_reads.reads() <= reads_after_claim + 1,
        "claimed worker added {} run-store reads after startup ownership settled",
        observer_reads.reads() - reads_after_claim
    );
    let stored = runtime.show_job_run(&run.run_id).expect("show claimed run");
    assert_eq!(stored.pid, Some(worker_pid));
    assert_eq!(stored.state, JobRunState::Pending);

    thread::sleep(Duration::from_secs(3));
    let terminal = wait_for_worker_terminal(&runtime, &run.run_id);
    assert_eq!(terminal.state, JobRunState::Interrupted);
    assert_child_reaped(worker_pid);
}

/// Exact worker ownership is a persisted state-machine invariant. Keep this
/// regression in-process: a detached child is unnecessary to prove that a
/// second PID cannot replace an already-running owner.
#[test]
fn duplicate_worker_cannot_replace_the_persisted_owner() {
    let (_root, runtime) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert pending run");
    let owner_pid = std::process::id();
    let duplicate_pid = owner_pid
        .checked_add(1)
        .expect("process id has a successor");

    assert!(
        runtime
            .stores()
            .jobs()
            .claim_pending_job_run_owner(&run.run_id, owner_pid)
            .expect("claim exact owner")
    );
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, Utc::now(), owner_pid)
            .expect("start exact owner"),
        JobRunStartOutcome::Started
    );

    let duplicate_start =
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, Utc::now(), duplicate_pid);
    assert!(
        matches!(duplicate_start, Err(OrbitError::JobRunStartConflict(_))),
        "duplicate worker must lose the atomic Start race: {duplicate_start:?}"
    );

    let stored = runtime.show_job_run(&run.run_id).expect("show owned run");
    assert_eq!(stored.state, JobRunState::Running);
    assert_eq!(stored.pid, Some(owner_pid));
    assert!(stored.finished_at.is_none());
    assert!(stored.steps.is_empty());
}

#[test]
fn mixed_crew_validation_after_start_terminalizes_without_admitting_tasks() {
    let (_root, runtime) = test_runtime_with_named_crews();
    let jobs_dir = runtime.paths().global_dir.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(
        jobs_dir.join("task_auto_pipeline.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: task_auto_pipeline
spec:
  state: enabled
  kind: workflow
  max_active_runs: 10
  steps:
    - id: unreachable
      spec:
        type: deterministic
        action: sleep
        config: {}
"#,
    )
    .expect("seed task_auto_pipeline definition");
    let terra = runtime
        .add_task(TaskAddParams {
            title: "Terra task".to_string(),
            description: "Mixed crew fixture".to_string(),
            crew: Some("terra".to_string()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add terra task");
    let sol = runtime
        .add_task(TaskAddParams {
            title: "Sol task".to_string(),
            description: "Mixed crew fixture".to_string(),
            crew: Some("sol".to_string()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add sol task");
    let input = serde_json::json!({ "task_ids": [terra.id, sol.id] });
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(input.clone()),
            None,
        )
        .expect("insert mixed-crew run");
    runtime
        .seed_v2_pipeline_run(&run, &input, None)
        .expect("seed pipeline state");

    let error = runtime
        .execute_pipeline_run_worker(&run.run_id)
        .expect_err("direct mixed-crew input must fail closed");
    let message = error.to_string();
    assert!(message.contains("mixes crews"), "{message}");
    assert!(message.contains("workflow.default_crew"), "{message}");

    let terminal = runtime.show_job_run(&run.run_id).expect("show failed run");
    assert_eq!(terminal.state, JobRunState::Failed);
    assert!(terminal.finished_at.is_some());
    assert!(terminal.resolved_crew.is_none());
    let diagnostic = terminal.steps.last().expect("failure diagnostic");
    assert!(
        diagnostic
            .error_message
            .as_deref()
            .is_some_and(|value| value.contains("mixes crews"))
    );
    for task_id in [&terra.id, &sol.id] {
        assert_eq!(
            runtime
                .get_task(task_id)
                .expect("task remains readable")
                .status,
            TaskStatus::Backlog,
            "mixed-crew validation must happen before task admission"
        );
    }

    let started = Instant::now();
    let waited = runtime
        .wait_pipeline_runs(std::slice::from_ref(&run.run_id), 10, 1, Some("test"))
        .expect("terminal child is immediately observable");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(waited.results[0].status, "failed");
    assert!(
        waited.results[0]
            .error
            .as_deref()
            .is_some_and(|value| value.contains("mixes crews"))
    );
}

/// Explicit shipment validates its selected task's effective crew before a
/// run can be persisted, then carries the canonical restriction into the
/// child pipeline for the dispatch-time provider gate.
#[test]
fn explicit_ship_crew_allowlist_admits_only_configured_permitted_crews() {
    let (_root, runtime) = test_runtime_with_named_crews();
    let _worker = WorkerOverride::shell("exit 0");
    let jobs_dir = runtime.paths().global_dir.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(
        jobs_dir.join("task_auto_pipeline.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: task_auto_pipeline
spec:
  state: enabled
  kind: workflow
  steps:
    - id: nap
      spec:
        type: deterministic
        action: sleep
        config: {}
"#,
    )
    .expect("seed task_auto_pipeline definition");
    let permitted = runtime
        .add_task(TaskAddParams {
            title: "Sol shipment".to_string(),
            description: "Explicit crew allowlist fixture".to_string(),
            crew: Some("sol".to_string()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add permitted task");
    let excluded = runtime
        .add_task(TaskAddParams {
            title: "Primary shipment".to_string(),
            description: "Explicit crew allowlist fixture".to_string(),
            crew: Some("primary".to_string()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add excluded task");

    let error = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            std::slice::from_ref(&excluded.id),
            CompletionPolicy::Review,
            &["sol".to_string()],
            Some("test"),
            None,
        )
        .expect_err("an excluded explicit crew must be refused before persistence");
    assert!(error.to_string().contains("primary"), "{error}");
    assert!(error.to_string().contains("sol"), "{error}");
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .expect("list runs")
            .is_empty(),
        "the excluded task must not create a run"
    );

    let unknown = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            std::slice::from_ref(&permitted.id),
            CompletionPolicy::Review,
            &["unknown".to_string()],
            Some("test"),
            None,
        )
        .expect_err("an unknown configured crew must fail before run creation");
    assert!(unknown.to_string().contains("unknown"), "{unknown}");
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .expect("list runs")
            .is_empty(),
        "an invalid allowlist must not create a run"
    );

    let admitted = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            std::slice::from_ref(&permitted.id),
            CompletionPolicy::Review,
            &["sol".to_string()],
            Some("test"),
            None,
        )
        .expect("the explicitly permitted singleton is submitted");
    let input = runtime
        .show_job_run(&admitted.run_id)
        .expect("show admitted run")
        .input
        .expect("persisted input");
    assert_eq!(input["allowed_crews"], serde_json::json!(["sol"]));
}

fn wait_for_worker_ownership_outcome(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> orbit_types::workflow::JobRun {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let stored = runtime
            .get_job_run_backend(run_id)
            .expect("read worker run")
            .expect("worker run exists");
        if stored.pid.is_some() || stored.state != JobRunState::Pending {
            return stored;
        }
        assert!(
            Instant::now() < deadline,
            "worker remained pending and unclaimed beyond ownership window"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_worker_terminal(runtime: &OrbitRuntime, run_id: &str) -> orbit_types::workflow::JobRun {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let stored = runtime
            .get_job_run_backend(run_id)
            .expect("read worker run")
            .expect("worker run exists");
        if stored.state.is_terminal() {
            return stored;
        }
        assert!(
            Instant::now() < deadline,
            "worker remained non-terminal beyond ownership window"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn assert_child_reaped(pid: u32) {
    let mut status = 0;
    // SAFETY: `waitpid` only inspects the explicitly spawned fixture PID and
    // writes to the valid local status pointer.
    let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    assert_eq!(result, -1, "worker {pid} is still a waitable child");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD),
        "worker {pid} must already have been reaped by the observer"
    );
}

/// Retry a pipeline audit lookup instead of asserting on a single snapshot:
/// the audit event is written after the run's terminal state and diagnostic
/// step, so an observer that only waits for those can still race the audit.
fn wait_for_pipeline_audit_event(
    runtime: &OrbitRuntime,
    status: Option<AuditEventStatus>,
    description: &str,
    predicate: impl Fn(&orbit_types::telemetry::AuditEvent) -> bool,
) -> orbit_types::telemetry::AuditEvent {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let audits = runtime
            .list_audit_events(None, None, status, None, 20)
            .expect("list pipeline audit events");
        if let Some(audit) = audits.into_iter().find(|audit| predicate(audit)) {
            return audit;
        }
        assert!(
            Instant::now() < deadline,
            "expected {description} was not persisted within the window"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_log_contains(path: &Path, expected: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let output = std::fs::read_to_string(path).expect("read durable worker log");
        if output.contains(expected) {
            return output;
        }
        assert!(
            Instant::now() < deadline,
            "worker log did not contain expected output: {expected}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn long_lived_worker_reopens_and_applies_compatible_pending_schema() {
    let (_root, runtime) = test_runtime();
    let store = runtime.sqlite_store().expect("open store fixture");
    {
        let connection = store.connection();
        let conn = connection.lock().expect("store connection");
        // Rewind past the newest ledger entry as well: the recorded version is
        // the highest key present, so leaving a later key behind would report
        // the store as current and apply nothing. Every entry from v0011 up is
        // therefore dropped, not a fixed list that a new migration would
        // silently outrun.
        conn.execute(
            "DELETE FROM schema_meta WHERE key LIKE 'migration.v%' AND key >= 'migration.v0011'",
            [],
        )
        .expect("rewind routine migration ledger");
        conn.execute_batch(
            "DROP TABLE routine_pauses;
             DROP TABLE routine_fires;
             DROP TABLE routine_cursors;
             DROP TABLE friction_import_state;
             DROP TABLE friction_record_tags;
             DROP TABLE friction_records;",
        )
        .expect("rewind routine schema");
    }

    runtime
        .preflight_pipeline_worker_store()
        .expect("compatible worker preflight");

    assert_eq!(
        store.schema_version().expect("schema after preflight"),
        SUPPORTED_SCHEMA_VERSION
    );
    let connection = store.connection();
    let conn = connection.lock().expect("store connection");
    let restored_tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name IN (
                 'routine_cursors', 'routine_fires', 'routine_pauses',
                 'friction_records', 'friction_record_tags', 'friction_import_state'
             )",
            [],
            |row| row.get(0),
        )
        .expect("count restored tables");
    assert_eq!(restored_tables, 6);
}

#[test]
fn newer_schema_fails_before_worker_claims_or_executes() {
    let (_root, runtime) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_gate_pipeline", 1, Utc::now(), None, None)
        .expect("insert pending run");
    let store = runtime.sqlite_store().expect("open store fixture");
    {
        let connection = store.connection();
        let conn = connection.lock().expect("store connection");
        conn.execute(
            "INSERT INTO schema_meta(key, value, updated_at)
             VALUES (?1, 'future_schema', '2099-01-01T00:00:00Z')",
            [format!(
                "migration.v{:04}",
                SUPPORTED_SCHEMA_VERSION.saturating_add(1)
            )],
        )
        .expect("advance store beyond worker");
    }

    let error = runtime
        .execute_pipeline_run_worker(&run.run_id)
        .expect_err("newer schema must fail worker preflight");
    assert!(matches!(error, OrbitError::Migration(_)), "{error:?}");

    let stored = runtime.show_job_run(&run.run_id).expect("show pending run");
    assert_eq!(stored.state, JobRunState::Pending);
    assert_eq!(stored.pid, None);
    assert!(stored.steps.is_empty());
}

#[test]
fn existing_pipeline_worker_executable_path_is_preserved() {
    let dir = TempDir::new().expect("tempdir");
    let executable = dir.path().join("orbit (deleted)");
    std::fs::write(&executable, "replacement").expect("write executable fixture");

    assert_eq!(
        resolve_pipeline_worker_executable(executable.clone()),
        executable
    );
}

#[cfg(target_os = "linux")]
#[test]
fn deleted_current_executable_resolves_to_replaced_installed_path() {
    let dir = TempDir::new().expect("tempdir");
    let installed = dir.path().join("orbit");
    std::fs::write(&installed, "replacement").expect("write replacement executable");
    let deleted_inode_path = installed.with_file_name("orbit (deleted)");

    assert!(
        !deleted_inode_path.exists(),
        "the kernel-style deleted-inode pseudo-path must be absent"
    );
    assert_eq!(
        resolve_pipeline_worker_executable(deleted_inode_path),
        installed,
        "the worker must launch through the replacement at the installed path"
    );
}

/// ORB-10544: the duplicate-dispatch guard lives in the shared submission path,
/// so it cannot be bypassed by a future adapter that calls `submit_ship_run`
/// directly instead of going through the dashboard endpoint or the MCP tool.
/// Asserted here against the shared entry point itself, with no HTTP or tool
/// surface in the picture.
#[test]
fn ship_submission_refuses_a_task_already_carried_by_a_non_terminal_run() {
    let (_root, runtime) = test_runtime();
    let selected_task_id = add_backlog_task(&runtime);
    let in_flight = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(serde_json::json!({"mode": "local", "task_ids": [selected_task_id]})),
            None,
        )
        .expect("insert in-flight run");
    assert!(!in_flight.state.is_terminal());

    let error = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            std::slice::from_ref(&selected_task_id),
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
        )
        .expect_err("a task with a run in flight must not dispatch a second run");

    let OrbitError::ShipRunInFlight {
        task_id: guarded_task_id,
        run_id,
    } = &error
    else {
        panic!("expected ShipRunInFlight, got {error:?}");
    };
    assert_eq!(guarded_task_id, &selected_task_id);
    assert_eq!(run_id, &in_flight.run_id);

    let runs = runtime
        .list_job_runs(JobRunListParams::default())
        .expect("list job runs");
    assert_eq!(
        runs.iter().map(|run| &run.run_id).collect::<Vec<_>>(),
        vec![&in_flight.run_id],
        "the refused submission must not persist another run"
    );
}

/// The shared guard is keyed on the explicit selection: an unrelated task is
/// still shippable while another one is in flight, and auto (backlog-discovery)
/// mode — which names no tasks — is never keyed and so never refused.
///
/// Neither call is expected to dispatch: this fixture seeds no job asset, so
/// both fall through the guard to the same job-not-found refusal. That the
/// refusal is *not* `ShipRunInFlight` is exactly the assertion, and it keeps the
/// test from spawning a detached pipeline worker.
#[test]
fn ship_submission_guard_is_scoped_to_the_selected_tasks() {
    let (_root, runtime) = test_runtime();
    let in_flight_task_id = add_backlog_task(&runtime);
    let unrelated_task_id = add_backlog_task(&runtime);
    runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(serde_json::json!({"mode": "local", "task_ids": [in_flight_task_id]})),
            None,
        )
        .expect("insert in-flight run");

    for (label, task_ids) in [
        ("an unrelated explicit task", vec![unrelated_task_id]),
        ("auto discovery", Vec::new()),
    ] {
        let error = runtime
            .submit_ship_run(
                ShipMode::Local,
                Some("main"),
                &task_ids,
                CompletionPolicy::Review,
                &[],
                Some("test"),
                None,
            )
            .expect_err("no job asset is deployed in this fixture");
        assert!(
            !matches!(error, OrbitError::ShipRunInFlight { .. }),
            "{label} must pass the in-flight guard: {error:?}"
        );
        assert!(
            matches!(error, OrbitError::NotFound { .. }),
            "{label} must fail on the missing job asset instead: {error:?}"
        );
    }
}

/// Explicit task validation belongs in the shared runtime path, ahead of
/// pipeline persistence, so every submission surface reports a typo directly
/// and cannot leave an orphaned worker/run behind.
#[test]
fn ship_submission_refuses_a_missing_explicit_task_before_persisting_a_run() {
    let (_root, runtime) = test_runtime();
    let missing_id = "ORB-99999".to_string();

    let error = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            std::slice::from_ref(&missing_id),
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
        )
        .expect_err("a missing explicit task must be rejected before dispatch");

    assert!(matches!(
        error,
        OrbitError::NotFound {
            kind: orbit_common::NotFoundKind::Task,
            id,
        } if id == missing_id
    ));
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .expect("list job runs")
            .is_empty(),
        "the refusal must not persist a run or spawn a worker"
    );
}

#[test]
fn ship_submission_refuses_an_epic_root_but_allows_its_child() {
    let (_root, runtime) = test_runtime();
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Epic root".to_string(),
            description: "Supervisor-owned fixture".to_string(),
            tags: vec!["epic".to_string()],
            ..Default::default()
        })
        .expect("create epic root");
    let child = runtime
        .add_task(TaskAddParams {
            parent_id: Some(epic.id.clone()),
            title: "Epic child".to_string(),
            description: "Leaf fixture".to_string(),
            ..Default::default()
        })
        .expect("create epic child");

    let error = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            std::slice::from_ref(&epic.id),
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
        )
        .expect_err("epic root must be refused before dispatch");
    assert!(matches!(error, OrbitError::InvalidInput(message) if message.contains("epic root")));
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .expect("list job runs")
            .is_empty(),
        "root refusal must happen before pipeline persistence"
    );

    let child_error = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            std::slice::from_ref(&child.id),
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
        )
        .expect_err("fixture intentionally has no deployed job asset");
    assert!(
        matches!(child_error, OrbitError::NotFound { .. }),
        "epic child must pass leaf admission and reach job lookup: {child_error:?}"
    );
}

#[test]
fn ship_submission_mixed_explicit_selection_identifies_the_missing_task() {
    let (_root, runtime) = test_runtime();
    let existing_id = add_backlog_task(&runtime);
    let missing_id = "ORB-99999".to_string();

    let error = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            &[existing_id, missing_id.clone()],
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
        )
        .expect_err("mixed selections must refuse their missing task before dispatch");

    assert!(matches!(
        error,
        OrbitError::NotFound {
            kind: orbit_common::NotFoundKind::Task,
            id,
        } if id == missing_id
    ));
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .expect("list job runs")
            .is_empty(),
        "mixed-selection refusal must not persist a run"
    );
}
