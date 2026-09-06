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
    resolve_pipeline_worker_executable,
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
        pipeline_worker_profile_file(Path::new("/tmp/logs"), "jrun-child", None),
        None
    );
    assert_eq!(
        pipeline_worker_profile_file(Path::new("/tmp/logs"), "jrun-child", Some(OsStr::new(""))),
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
        ),
        Some(PathBuf::from("/tmp/logs/jrun-child.%p.profraw"))
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
    let log_path =
        configure_pipeline_worker_stdio(&mut command, &runtime.paths().logs_dir, &run.run_id)
            .expect("configure worker log");

    runtime
        .spawn_pipeline_worker_process(&run.run_id, Some("test"), command, log_path.clone())
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
    let log_path =
        configure_pipeline_worker_stdio(&mut command, &runtime.paths().logs_dir, &run.run_id)
            .expect("configure routine worker log");

    let started = Instant::now();
    let worker_pid = runtime
        .spawn_pipeline_worker_process(
            &run.run_id,
            Some("routine-sweep"),
            command,
            log_path.clone(),
        )
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

/// [ORB-11116] Two observers can watch children for the same persisted run.
/// The duplicate exits zero after losing Start, but only the child whose exact
/// PID is persisted may be treated as the owner by its observer.
#[cfg(unix)]
#[test]
fn duplicate_worker_exit_leaves_real_owner_authoritative_and_non_terminal() {
    let (_root, runtime) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert pending run");
    let owner_release = runtime.paths().logs_dir.join("release-real-owner");

    let mut owner_command = Command::new("sh");
    owner_command.env("ORBIT_TEST_OWNER_RELEASE", &owner_release);
    owner_command.args([
        "-c",
        "while [ ! -f \"$ORBIT_TEST_OWNER_RELEASE\" ]; do sleep 0.01; done; exit 0",
    ]);
    let owner_log = configure_pipeline_worker_stdio(
        &mut owner_command,
        &runtime.paths().logs_dir,
        &format!("{}-owner", run.run_id),
    )
    .expect("configure owner worker log");
    let owner_pid = runtime
        .spawn_pipeline_worker_process(&run.run_id, Some("test"), owner_command, owner_log)
        .expect("spawn real owner fixture");
    assert!(
        runtime
            .stores()
            .jobs()
            .claim_pending_job_run_owner(&run.run_id, owner_pid)
            .expect("claim real owner")
    );
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, Utc::now(), owner_pid)
            .expect("start real owner"),
        JobRunStartOutcome::Started
    );

    let claimed = wait_for_pipeline_audit_event(&runtime, None, "exact-owner audit", |audit| {
        audit.tool_name.as_deref() == Some("pipeline.worker.claimed")
            && audit.target_id.as_deref() == Some(run.run_id.as_str())
    });
    let claimed_arguments: serde_json::Value = serde_json::from_str(
        claimed
            .arguments_json
            .as_deref()
            .expect("claimed audit arguments"),
    )
    .expect("parse claimed audit arguments");
    assert_eq!(claimed_arguments["worker_pid"], owner_pid);
    assert_eq!(claimed_arguments["owner_pid"], owner_pid);

    let duplicate_release = runtime.paths().logs_dir.join("release-duplicate-worker");
    let mut duplicate_command = Command::new("sh");
    duplicate_command.env("ORBIT_TEST_DUPLICATE_RELEASE", &duplicate_release);
    duplicate_command.args([
        "-c",
        "while [ ! -f \"$ORBIT_TEST_DUPLICATE_RELEASE\" ]; do sleep 0.01; done; exit 0",
    ]);
    let duplicate_log = configure_pipeline_worker_stdio(
        &mut duplicate_command,
        &runtime.paths().logs_dir,
        &format!("{}-duplicate", run.run_id),
    )
    .expect("configure duplicate worker log");
    let duplicate_pid = runtime
        .spawn_pipeline_worker_process(&run.run_id, Some("test"), duplicate_command, duplicate_log)
        .expect("spawn duplicate worker fixture");
    let duplicate_start =
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, Utc::now(), duplicate_pid);
    assert!(
        matches!(duplicate_start, Err(OrbitError::JobRunStartConflict(_))),
        "duplicate worker must lose the atomic Start race: {duplicate_start:?}"
    );
    std::fs::write(&duplicate_release, "release").expect("release duplicate worker");

    let duplicate =
        wait_for_pipeline_audit_event(&runtime, None, "duplicate-worker audit", |audit| {
            audit.tool_name.as_deref() == Some("pipeline.worker.duplicate")
                && audit.target_id.as_deref() == Some(run.run_id.as_str())
        });
    let duplicate_arguments: serde_json::Value = serde_json::from_str(
        duplicate
            .arguments_json
            .as_deref()
            .expect("duplicate audit arguments"),
    )
    .expect("parse duplicate audit arguments");
    assert_eq!(duplicate_arguments["worker_pid"], duplicate_pid);
    assert_eq!(duplicate_arguments["owner_pid"], owner_pid);
    assert_eq!(duplicate_arguments["exit_status"], "exit status: 0");
    assert_child_reaped(duplicate_pid);

    let after_duplicate = runtime
        .show_job_run(&run.run_id)
        .expect("show run after duplicate exit");
    assert_eq!(after_duplicate.state, JobRunState::Running);
    assert_eq!(after_duplicate.pid, Some(owner_pid));
    assert!(after_duplicate.finished_at.is_none());
    assert!(after_duplicate.steps.is_empty());

    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, JobRunState::Success, Utc::now(), Some(1))
        .expect("real owner completes run");
    std::fs::write(&owner_release, "release").expect("release real owner");

    let completed = runtime
        .show_job_run(&run.run_id)
        .expect("show completed run");
    assert_eq!(completed.state, JobRunState::Success);
    assert!(completed.finished_at.is_some());
    assert!(completed.steps.is_empty());
    let false_owner_exit = runtime
        .list_audit_events(None, None, None, None, 50)
        .expect("list worker audits")
        .into_iter()
        .any(|audit| {
            audit.tool_name.as_deref() == Some("pipeline.worker.exit")
                && audit.target_id.as_deref() == Some(run.run_id.as_str())
        });
    assert!(
        !false_owner_exit,
        "duplicate exit must not be an owner failure"
    );
}

#[cfg(unix)]
#[test]
fn worker_exit_after_mark_running_is_reaped_and_terminalizes_the_run() {
    let (_root, runtime) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert pending run");
    let release_path = runtime.paths().logs_dir.join("release-claimed-worker");
    let mut command = Command::new("sh");
    command.env("ORBIT_TEST_WORKER_RELEASE", &release_path);
    command.args([
        "-c",
        "while [ ! -f \"$ORBIT_TEST_WORKER_RELEASE\" ]; do sleep 0.01; done; \
         printf 'post-claim validation exploded\\n' >&2; exit 29",
    ]);
    let log_path =
        configure_pipeline_worker_stdio(&mut command, &runtime.paths().logs_dir, &run.run_id)
            .expect("configure worker log");
    let worker_pid = runtime
        .spawn_pipeline_worker_process(&run.run_id, Some("test"), command, log_path)
        .expect("spawn claimed worker fixture");
    runtime
        .stores()
        .jobs()
        .claim_pending_job_run_owner(&run.run_id, worker_pid)
        .expect("claim run owner");
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, Utc::now(), worker_pid)
            .expect("mark run running"),
        JobRunStartOutcome::Started
    );
    assert_eq!(
        runtime
            .show_job_run(&run.run_id)
            .expect("show running fixture")
            .state,
        JobRunState::Running
    );
    std::fs::write(&release_path, "release").expect("release claimed worker");

    let terminal = wait_for_worker_terminal(&runtime, &run.run_id);
    let message = terminal
        .steps
        .last()
        .and_then(|step| step.error_message.as_deref())
        .expect("post-claim diagnostic");
    assert_eq!(terminal.state, JobRunState::Failed, "{message}");
    assert!(terminal.finished_at.is_some());
    assert!(message.contains("after claiming"), "{message}");
    assert!(message.contains("exit status: 29"), "{message}");
    assert!(
        message.contains("post-claim validation exploded"),
        "{message}"
    );
    assert_child_reaped(worker_pid);
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
