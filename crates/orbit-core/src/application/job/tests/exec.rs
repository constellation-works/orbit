use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod already_landed;
mod ci_sweep;
mod completion;
mod local_ship;
mod review_gate;

use chrono::Utc;
use orbit_engine::{
    DispatchError, JobOutcome, ResolvedCliExecutor, RuntimeHost, V2AuditWriter,
    execute_job_with_resume, resolve_job_catalog_refs_for_execution,
};
use orbit_store::{InvocationQuery, V2AuditEventFilter};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    ActivityV2Spec, ExecutorDef, ExecutorType, JobRunState, JobV2Step, JobV2StepBody,
};
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::SYSTEM_AUDIT_IDENTITY;
use crate::application::job::pipeline::{PIPELINE_WAIT_MAX_TIMEOUT_SECONDS, PipelineWaitClock};
use crate::application::job::seed_default_jobs;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::bootstrap::activity::seed_default_activities;

pub(super) fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root, global_root)
}

pub(super) fn test_runtime_with_workspace_config(
    config: &str,
) -> (tempfile::TempDir, OrbitRuntime, PathBuf, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(workspace_root.join("config.toml"), config).expect("write workspace config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root, global_root)
}

pub(super) fn seed_default_catalogs(global_root: &Path) {
    seed_default_activities(&global_root.join("resources/activities"), true)
        .expect("seed default activities");
    seed_default_jobs(&global_root.join("resources/jobs"), true).expect("seed default jobs");
}

fn write_context_file(repo_root: &Path, relative_path: &str) {
    let path = repo_root.join(relative_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create context parent");
    }
    std::fs::write(path, "fixture\n").expect("write context file");
}

pub(super) fn seed_gate_task(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    status: TaskStatus,
) -> String {
    write_context_file(repo_root, "src/lib.rs");
    runtime
        .add_task(TaskAddParams {
            title: format!("Gate fixture {status}"),
            description: "Fixture task for task_gate_pipeline admission.".to_string(),
            acceptance_criteria: vec!["Gate behavior is observable.".to_string()],
            plan: "Fixture execution plan.".to_string(),
            context_files: vec!["src/lib.rs".to_string()],
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(status),
            ..Default::default()
        })
        .expect("seed gate task")
        .id
}

pub(super) fn resolved_job(
    runtime: &OrbitRuntime,
    job_name: &str,
) -> orbit_types::workflow::activity_job::JobV2 {
    let (_path, mut job) = runtime
        .load_v2_job_asset_by_name(job_name)
        .unwrap_or_else(|err| panic!("load {job_name}: {err}"));
    let catalog = runtime.v2_activity_catalog().expect("activity catalog");
    resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .unwrap_or_else(|err| panic!("resolve {job_name} activities: {err}"));
    job
}

fn execute_gate_job(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    host: &dyn RuntimeHost,
    input: Value,
    run_id: &str,
) -> JobOutcome {
    try_execute_gate_job(runtime, repo_root, host, input, run_id).expect("execute gate job")
}

fn try_execute_gate_job(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    host: &dyn RuntimeHost,
    input: Value,
    run_id: &str,
) -> Result<JobOutcome, DispatchError> {
    try_execute_named_job(
        runtime,
        repo_root,
        host,
        "task_gate_pipeline",
        input,
        run_id,
    )
}

pub(super) fn try_execute_named_job(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    host: &dyn RuntimeHost,
    job_name: &str,
    input: Value,
    run_id: &str,
) -> Result<JobOutcome, DispatchError> {
    let job = resolved_job(runtime, job_name);
    try_execute_job(runtime, repo_root, host, job, input, run_id)
}

pub(super) fn try_execute_job(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    host: &dyn RuntimeHost,
    job: orbit_types::workflow::activity_job::JobV2,
    input: Value,
    run_id: &str,
) -> Result<JobOutcome, DispatchError> {
    let writer = V2AuditWriter::with_disk_sinks(
        &runtime.paths().audit_dir,
        runtime
            .v2_audit_store()
            .map_err(|err| DispatchError::JobExecution(format!("open audit store: {err}")))?,
        runtime
            .workspace_id()
            .map_err(|err| DispatchError::JobExecution(format!("resolve workspace id: {err}")))?,
        run_id,
        SYSTEM_AUDIT_IDENTITY,
        Some(repo_root),
    )
    .expect("audit writer");
    execute_job_with_resume(&job, input, run_id, writer, host, None)
}

pub(super) fn git_in(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const WORKTREE_FIXTURE_FAILURE: &str = "fixture: worktree setup exploded";

/// Runs `task_pr_pipeline` against the real runtime dispatch table with a
/// seeded first-step failure, recording how every deterministic action
/// resolved so the terminal failure hook's fate is observable — the
/// executor deliberately swallows that hook's own error.
struct FailureHandoffHost<'a> {
    runtime: &'a OrbitRuntime,
    dispatches: Mutex<Vec<(String, String)>>,
}

impl FailureHandoffHost<'_> {
    fn new(runtime: &OrbitRuntime) -> FailureHandoffHost<'_> {
        FailureHandoffHost {
            runtime,
            dispatches: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, action: &str, outcome: String) {
        self.dispatches
            .lock()
            .expect("dispatch log")
            .push((action.to_string(), outcome));
    }

    fn outcomes(&self, action: &str) -> Vec<String> {
        self.dispatches
            .lock()
            .expect("dispatch log")
            .iter()
            .filter(|(recorded, _)| recorded == action)
            .map(|(_, outcome)| outcome.clone())
            .collect()
    }
}

impl RuntimeHost for FailureHandoffHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        if action == "test_worktree_setup" {
            self.record(action, "seeded_failure".to_string());
            return Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: WORKTREE_FIXTURE_FAILURE.to_string(),
            });
        }
        let result = <OrbitRuntime as RuntimeHost>::run_deterministic(
            self.runtime,
            action,
            config,
            input,
            tool_context,
        );
        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(DispatchError::DeterministicActionNotRegistered(name)) => {
                format!("not_registered: {name}")
            }
            Err(other) => format!("dispatched: {other}"),
        };
        self.record(action, outcome);
        result
    }

    fn has_deterministic_action(&self, action: &str) -> bool {
        if action == "test_worktree_setup" {
            return true;
        }
        <OrbitRuntime as RuntimeHost>::has_deterministic_action(self.runtime, action)
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        <OrbitRuntime as RuntimeHost>::resolve_cli_executor(self.runtime, provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            self.runtime,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

/// [ORB-10410] `task_pr_pipeline` binds `pr_failure_handoff` as its
/// terminal `failure_activity`, and that hook resolves through the same v2
/// deterministic dispatch table as any ordinary step. While the allowlist
/// omitted the name, every terminal PR-pipeline failure ran the hook as
/// `DeterministicActionNotRegistered`: the recoverable candidate was never
/// published, and the registry miss masked the real failure. The hook must
/// reach its own implementation, and the original step error must stay
/// authoritative.
#[test]
fn failed_pr_pipeline_dispatches_the_failure_handoff_and_keeps_the_original_error() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let run_id = Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .to_string();
    let worktree_activity = global_root.join("resources/activities/worktree_setup.yaml");
    let activity_yaml = std::fs::read_to_string(&worktree_activity)
        .expect("read seeded worktree activity")
        .replace("action: worktree_setup", "action: test_worktree_setup");
    std::fs::write(&worktree_activity, activity_yaml).expect("write test worktree activity");
    let host = FailureHandoffHost::new(&runtime);

    let error = try_execute_named_job(
        &runtime,
        &repo_root,
        &host,
        "task_pr_pipeline",
        json!({
            "task_ids": ["ORB-FAILURE-HANDOFF"],
            "base_branch": "agent-main",
            "base_sync": "local",
            "review": false,
        }),
        &run_id,
    )
    .expect_err("the seeded worktree failure must terminalize the run");

    assert!(
        error.to_string().contains(WORKTREE_FIXTURE_FAILURE),
        "the original step error stays authoritative: {error}"
    );

    assert!(
        host.outcomes("test_worktree_setup")
            .iter()
            .any(|outcome| outcome == "seeded_failure"),
        "the test action must reach the host once before terminal failure handling"
    );
}

struct ReserveThenStatusHost<'a> {
    runtime: &'a OrbitRuntime,
    task_id: String,
    status_after_reserve: TaskStatus,
    reserve_calls: AtomicUsize,
}

impl ReserveThenStatusHost<'_> {
    fn reserve_calls(&self) -> usize {
        self.reserve_calls.load(Ordering::SeqCst)
    }
}

impl RuntimeHost for ReserveThenStatusHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        let output = <OrbitRuntime as RuntimeHost>::run_deterministic(
            self.runtime,
            action,
            config,
            input,
            tool_context,
        )?;
        if action == "reserve_locks" && output["reserved"] == json!(true) {
            self.reserve_calls.fetch_add(1, Ordering::SeqCst);
            self.runtime
                .update_task(
                    &self.task_id,
                    TaskUpdateParams {
                        status: Some(self.status_after_reserve),
                        execution_summary: (self.status_after_reserve == TaskStatus::Review)
                            .then(|| "Fixture reached review in a competing run.".to_string()),
                        ..Default::default()
                    },
                )
                .map_err(|err| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("test status flip failed: {err}"),
                })?;
        }
        Ok(output)
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        <OrbitRuntime as RuntimeHost>::resolve_cli_executor(self.runtime, provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            self.runtime,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

struct ScriptedGateHost<'a> {
    runtime: &'a OrbitRuntime,
    child_status: &'static str,
    call_log: Mutex<Vec<String>>,
}

impl ScriptedGateHost<'_> {
    fn new<'a>(runtime: &'a OrbitRuntime, child_status: &'static str) -> ScriptedGateHost<'a> {
        ScriptedGateHost {
            runtime,
            child_status,
            call_log: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self, action: &str) -> usize {
        self.call_log
            .lock()
            .expect("call log")
            .iter()
            .filter(|recorded| recorded.as_str() == action)
            .count()
    }
}

impl RuntimeHost for ScriptedGateHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        self.call_log
            .lock()
            .expect("call log")
            .push(action.to_string());
        match action {
            "reserve_locks" => Ok(json!({
                "reserved": true,
                "reservation_id": "reservation-scripted",
                "reserved_files": ["file:src/lib.rs"],
            })),
            "invoke_and_wait" => Ok(json!({
                "run_id": "jrun-scripted-child",
                "status": self.child_status,
                "error": (self.child_status != "succeeded")
                    .then_some("scripted child failure"),
            })),
            "release_locks" => Ok(json!({ "released": true })),
            "pipeline_success_guard" => <OrbitRuntime as RuntimeHost>::run_deterministic(
                self.runtime,
                action,
                config,
                input,
                tool_context,
            ),
            other => Err(DispatchError::DeterministicActionNotRegistered(
                other.to_string(),
            )),
        }
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        <OrbitRuntime as RuntimeHost>::resolve_cli_executor(self.runtime, provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            self.runtime,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

fn write_job(path: &Path, name: &str, action: &str) {
    let yaml = format!(
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
        action: {action}
        config: {{}}
"#
    );
    std::fs::write(path, yaml).expect("write job yaml");
}

fn write_cli_metrics_job(path: &Path, name: &str, step_id: &str, provider: &str, model: &str) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  steps:
    - id: {step_id}
      spec:
        type: agent_loop
        instruction: "emit a successful Orbit envelope"
        tools: [orbit.task.show]
        on_denial: terminate
        max_iterations: 1
        model: {model}
        backend: cli
        provider: {provider}
        wall_clock_timeout_seconds: 30
"#
    );
    std::fs::write(path, yaml).expect("write cli metrics job yaml");
}

pub(super) fn v2_events(
    runtime: &OrbitRuntime,
    run_id: &str,
    event_type: &str,
) -> Vec<orbit_store::V2AuditEventRow> {
    runtime
        .list_v2_audit_events(V2AuditEventFilter {
            workspace_id: String::new(),
            run_id: Some(run_id.to_string()),
            event_type: Some(event_type.to_string()),
            ..Default::default()
        })
        .expect("list v2 audit events")
}

#[test]
fn task_gate_noops_when_task_reaches_review_after_reservation() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let task_id = seed_gate_task(&runtime, &repo_root, TaskStatus::Backlog);
    let host = ReserveThenStatusHost {
        runtime: &runtime,
        task_id: task_id.clone(),
        status_after_reserve: TaskStatus::Review,
        reserve_calls: AtomicUsize::new(0),
    };

    // The production retry path uses the run ID as a jitter salt. Reuse the
    // store-generated fixture task ID here so the test does not feed a
    // hard-coded value into that cryptographic data flow.
    let outcome = execute_gate_job(
        &runtime,
        &repo_root,
        &host,
        json!({
            "task_ids": [task_id.clone()],
            "mode": "pr",
        }),
        task_id.as_str(),
    );

    assert!(outcome.success);
    assert_eq!(host.reserve_calls(), 1);
    assert!(
        runtime
            .job_history("task_pr_pipeline")
            .expect("child history")
            .is_empty(),
        "stale gate must not submit a child PR pipeline"
    );
    let dispatch = &outcome.pipeline["dispatch_child"];
    assert_eq!(dispatch["status"], json!("success"));
    assert_eq!(dispatch["skipped"], json!(true));
    let reason = dispatch["reason"].as_str().expect("stale reason");
    assert!(reason.contains(&task_id), "{reason}");
    assert!(reason.contains("review"), "{reason}");

    let audit_events = runtime
        .list_audit_events(None, None, Some(AuditEventStatus::Success), None, 32)
        .expect("audit events");
    assert!(audit_events.iter().any(|event| {
        event.command == "gate.stale_noop"
            && event
                .arguments_json
                .as_deref()
                .is_some_and(|payload| payload.contains(&task_id) && payload.contains("review"))
    }));
}

#[test]
fn task_gate_noops_done_task_and_releases_reservation() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let task_id = seed_gate_task(&runtime, &repo_root, TaskStatus::Done);

    // The production retry path uses the run ID as a jitter salt. Reuse the
    // store-generated fixture task ID here so the test does not feed a
    // hard-coded value into that cryptographic data flow.
    let outcome = execute_gate_job(
        &runtime,
        &repo_root,
        &runtime,
        json!({
            "task_ids": [task_id.clone()],
            "mode": "pr",
        }),
        task_id.as_str(),
    );

    assert!(outcome.success);
    assert!(
        runtime
            .job_history("task_pr_pipeline")
            .expect("child history")
            .is_empty(),
        "done stale gate must not submit a child PR pipeline"
    );
    assert_eq!(outcome.pipeline["dispatch_child"]["skipped"], json!(true));
    let reason = outcome.pipeline["dispatch_child"]["reason"]
        .as_str()
        .expect("done stale reason");
    assert!(reason.contains(&task_id), "{reason}");
    assert!(reason.contains("done"), "{reason}");

    let locks = runtime
        .run_tool_with_context_and_role(
            "orbit.task.locks",
            json!({}),
            orbit_types::policy::Role::Admin,
            ToolContext::default(),
        )
        .expect("list locks");
    assert_eq!(locks["total_reservations"], json!(0));
}

#[test]
fn task_gate_dispatches_child_for_admissible_task() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let host = ScriptedGateHost::new(&runtime, "succeeded");
    let run_id = Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .to_string();

    let outcome = execute_gate_job(
        &runtime,
        &repo_root,
        &host,
        json!({
            "task_ids": ["ORB-SCRIPTED"],
            "mode": "pr",
        }),
        &run_id,
    );

    assert!(outcome.success);
    assert_eq!(host.call_count("reserve_locks"), 1);
    assert_eq!(host.call_count("invoke_and_wait"), 1);
    assert_eq!(host.call_count("release_locks"), 1);
    assert_eq!(host.call_count("pipeline_success_guard"), 1);
    assert_eq!(
        outcome.pipeline["dispatch_child"]["status"],
        json!("succeeded")
    );
}

#[test]
fn task_gate_child_failure_still_fails_success_guard() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let host = ScriptedGateHost::new(&runtime, "failed");
    let run_id = Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .to_string();

    let err = try_execute_gate_job(
        &runtime,
        &repo_root,
        &host,
        json!({
            "task_ids": ["ORB-SCRIPTED"],
            "mode": "pr",
        }),
        &run_id,
    )
    .expect_err("failed child should fail the gate");

    assert_eq!(host.call_count("invoke_and_wait"), 1);
    assert_eq!(host.call_count("release_locks"), 1);
    assert_eq!(host.call_count("pipeline_success_guard"), 1);
    let message = err.to_string();
    assert!(
        message.contains("task_gate_pipeline child run"),
        "{message}"
    );
    assert!(message.contains("jrun-scripted-child"), "{message}");
    assert!(message.contains("status failed"), "{message}");
}

/// Rename the engine's own deterministic actions so a scripted host sees
/// them. The v2 dispatcher routes a known engine action straight into
/// `execute_engine_action`, never through `RuntimeHost::run_deterministic`,
/// so a host override alone cannot intercept `worktree_setup`, the `git_*`
/// actions, or `update_task`.
pub(super) fn retarget_engine_actions_for_scripted_host(
    job: &mut orbit_types::workflow::activity_job::JobV2,
) {
    fn walk(step: &mut JobV2Step) {
        match &mut step.body {
            JobV2StepBody::Target(target) => {
                if let ActivityV2Spec::Deterministic(spec) = &mut target.spec {
                    match spec.action.as_str() {
                        "worktree_setup" | "git_commit" | "git_rebase" | "git_push"
                        | "git_merge" | "pr_prepare" | "pr_open" | "pr_promote" | "update_task" => {
                            spec.action = format!("scripted_{}", spec.action);
                        }
                        _ => {}
                    }
                }
            }
            JobV2StepBody::Loop { loop_ } => {
                for child in &mut loop_.steps {
                    walk(child);
                }
            }
            JobV2StepBody::Parallel { parallel } => {
                for child in &mut parallel.branches {
                    walk(child);
                }
            }
            JobV2StepBody::FanOut { fan_out, .. } => walk(&mut fan_out.worker),
            JobV2StepBody::TargetRef(_) => {}
        }
    }
    for step in &mut job.steps {
        walk(step);
    }
}

#[cfg(unix)]
fn write_fake_codex(path: &Path) {
    write_fake_cli_response(
        path,
        r#"{"type":"thread.started","thread_id":"fake"}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"orbit --version","aggregated_output":"","exit_code":null,"status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"orbit --version","aggregated_output":"ok","exit_code":0,"status":"completed"}}
{"schemaVersion":1,"status":"success","result":{"ok":true},"error":null}
{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":25,"output_tokens":12}}
"#,
    );
}

#[cfg(unix)]
fn write_fake_cli_response(path: &Path, stdout: &str) {
    use std::os::unix::fs::PermissionsExt;

    let output_path = path.with_extension("stdout");
    std::fs::write(&output_path, stdout).expect("write fake cli stdout");
    std::fs::write(path, "#!/bin/sh\ncat >/dev/null\ncat \"$0.stdout\"\n").expect("write fake cli");
    let mut permissions = std::fs::metadata(path)
        .expect("fake cli metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("chmod fake cli");
}

#[test]
fn direct_yaml_run_persists_history_and_run_state() {
    let (_root, runtime, repo_root, _global_root) = test_runtime();
    let yaml_path = repo_root.join("qa_sleep.yaml");
    write_job(&yaml_path, "qa_sleep", "sleep");

    let result = runtime
        .run_job_v2_from_yaml(&yaml_path, json!({ "seconds": 0 }))
        .expect("direct job run succeeds");

    let run = runtime.show_job_run(&result.run_id).expect("stored run");
    assert_eq!(run.job_id, "qa_sleep");
    assert_eq!(run.state, JobRunState::Success);
    assert_eq!(run.steps.len(), 1);

    let history = runtime.job_history("qa_sleep").expect("job history");
    assert!(history.iter().any(|run| run.run_id == result.run_id));

    let state = runtime
        .read_run_state(&result.run_id)
        .expect("read run state")
        .expect("persisted run state");
    assert_eq!(state.run_id, result.run_id);
    assert!(state.pipeline.get("nap").is_some());
    assert!(state.step_outputs.contains_key(&0));

    let first_event: serde_json::Value = serde_json::from_str(
        &v2_events(&runtime, &result.run_id, "run.started")
            .first()
            .expect("run.started audit event")
            .payload_json,
    )
    .expect("parse first audit event");
    assert_eq!(
        first_event
            .get("agent_identity")
            .and_then(serde_json::Value::as_str),
        Some(SYSTEM_AUDIT_IDENTITY)
    );
    assert!(!repo_root.join(".orbit/audit").exists());
}

#[test]
fn direct_catalog_run_is_visible_in_history() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    let yaml_path = jobs_dir.join("qa_catalog_sleep.yaml");
    write_job(&yaml_path, "qa_catalog_sleep", "sleep");

    let catalog = runtime
        .show_job_catalog_entry("qa_catalog_sleep")
        .expect("catalog entry");
    let result = runtime
        .run_job_v2_from_yaml(&catalog.path, json!({ "seconds": 0 }))
        .expect("catalog job run succeeds");

    let history = runtime
        .job_history("qa_catalog_sleep")
        .expect("catalog history");
    assert!(history.iter().any(|run| run.run_id == result.run_id));
    assert!(
        runtime
            .show_job_run(&result.run_id)
            .expect("stored run")
            .run_id
            == result.run_id
    );
}

#[test]
fn replay_job_run_records_lineage_and_preserves_source_bundle() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    let yaml_path = jobs_dir.join("qa_replay_sleep.yaml");
    write_job(&yaml_path, "qa_replay_sleep", "sleep");

    let catalog = runtime
        .show_job_catalog_entry("qa_replay_sleep")
        .expect("catalog entry");
    let input = json!({ "seconds": 0, "marker": "source-input" });
    let source_result = runtime
        .run_job_v2_from_yaml(&catalog.path, input.clone())
        .expect("source run succeeds");
    let source_run = runtime
        .show_job_run(&source_result.run_id)
        .expect("show source");
    let before = source_run.clone();

    let replay_result = runtime
        .replay_job_run(&source_result.run_id)
        .expect("replay succeeds");

    assert_ne!(replay_result.run_id, source_result.run_id);
    assert!(replay_result.success);
    let replay_run = runtime
        .show_job_run(&replay_result.run_id)
        .expect("show replay");
    assert_eq!(replay_run.job_id, source_run.job_id);
    assert_eq!(replay_run.input, Some(input));
    assert_eq!(
        replay_run.retry_source_run_id.as_deref(),
        Some(source_result.run_id.as_str())
    );
    assert_eq!(
        runtime
            .show_job_run(&source_run.run_id)
            .expect("show source after replay"),
        before
    );

    let event: serde_json::Value = serde_json::from_str(
        &v2_events(&runtime, &replay_result.run_id, "run.started")
            .first()
            .expect("run_started audit event")
            .payload_json,
    )
    .expect("parse run_started");
    assert_eq!(
        event
            .get("retry_source_run_id")
            .and_then(serde_json::Value::as_str),
        Some(source_result.run_id.as_str())
    );
}

/// [ORB-12255] Detached workers used to leave `JobRun::steps` empty. The
/// worker path now persists the step summary, and `--wait` uses the same
/// `success` token and `duration_ms` as `run show`.
#[test]
fn worker_path_persists_steps_and_wait_agrees_on_state_and_duration() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    let yaml_path = jobs_dir.join("qa_worker_steps.yaml");
    write_job(&yaml_path, "qa_worker_steps", "sleep");

    let input = json!({ "seconds": 0 });
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("qa_worker_steps", 1, Utc::now(), Some(input.clone()), None)
        .expect("insert worker run");
    runtime
        .seed_v2_pipeline_run(
            &run,
            &input,
            None,
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect("seed worker run");
    runtime
        .execute_pipeline_run_worker(&run.run_id)
        .expect("worker executes");

    let stored = runtime.show_job_run(&run.run_id).expect("show worker run");
    assert_eq!(stored.state, JobRunState::Success);
    assert!(
        !stored.steps.is_empty(),
        "worker-spawned run must persist steps, got none"
    );
    assert!(
        stored.duration_ms.is_some(),
        "worker-spawned run must record duration_ms"
    );

    let waited = runtime
        .wait_pipeline_runs(std::slice::from_ref(&run.run_id), 1, 1, Some("test"))
        .expect("wait on terminal run");
    assert_eq!(waited.results[0].status, "success");
    assert_eq!(waited.results[0].duration_ms, stored.duration_ms);

    let state = runtime
        .read_run_state(&stored.run_id)
        .expect("read state")
        .expect("pipeline state");
    let projection = crate::application::job::job_run_to_json(&stored, Some(&state));
    assert_eq!(projection["state"], "success");
    assert_eq!(projection["duration_ms"], json!(stored.duration_ms));
}

struct FastPipelineWaitClock<'a> {
    elapsed: Cell<Duration>,
    completion_at: Option<Duration>,
    runtime: &'a OrbitRuntime,
    run_id: &'a str,
    completed: Cell<bool>,
}

impl FastPipelineWaitClock<'_> {
    fn new<'a>(
        runtime: &'a OrbitRuntime,
        run_id: &'a str,
        completion_at: Option<Duration>,
    ) -> FastPipelineWaitClock<'a> {
        FastPipelineWaitClock {
            elapsed: Cell::new(Duration::ZERO),
            completion_at,
            runtime,
            run_id,
            completed: Cell::new(false),
        }
    }
}

impl PipelineWaitClock for FastPipelineWaitClock<'_> {
    fn elapsed(&self) -> Duration {
        self.elapsed.get()
    }

    fn sleep(&self, duration: Duration) {
        let elapsed = self.elapsed.get() + duration;
        self.elapsed.set(elapsed);

        if !self.completed.get() && self.completion_at.is_some_and(|limit| elapsed >= limit) {
            self.runtime
                .stores()
                .jobs()
                .finalize_job_run(
                    self.run_id,
                    JobRunState::Success,
                    Utc::now(),
                    Some(elapsed.as_millis() as u64),
                )
                .expect("fast-clock child completion");
            self.completed.set(true);
        }
    }
}

fn running_wait_fixture(runtime: &OrbitRuntime, job_name: &str) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(job_name, 1, Utc::now(), None, None)
        .expect("insert wait fixture");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start wait fixture");
    run.run_id
}

/// The accelerated clock covers the two formerly impossible production
/// timelines without turning this into a multi-hour test: an auto parent sees
/// admission near the one-hour limit plus delivery beyond its old one-hour
/// wait, and a gate sees the supported three-hour activity plus its bounded
/// delivery tail.
#[test]
fn nested_pipeline_wait_budgets_keep_healthy_long_running_children_successful() {
    let (_root, runtime, _repo_root, _global_root) = test_runtime();
    let scenarios = [
        ("task_gate_pipeline", 3_590 + 7_200, 21_600),
        ("task_pr_pipeline", 10_800 + 3_590, 14_400),
    ];

    for (job_name, completion_at, timeout) in scenarios {
        let run_id = running_wait_fixture(&runtime, job_name);
        let clock =
            FastPipelineWaitClock::new(&runtime, &run_id, Some(Duration::from_secs(completion_at)));
        let waited = runtime
            .wait_pipeline_runs_with_clock(
                std::slice::from_ref(&run_id),
                timeout,
                30,
                Some("fast-clock-test"),
                &clock,
            )
            .expect("healthy child completes within composed parent budget");

        assert_eq!(waited.results[0].run_id, run_id);
        assert_eq!(waited.results[0].status, "success");
        assert!(clock.elapsed() > Duration::from_secs(3_600));
    }
}

#[test]
fn configured_pipeline_wait_deadline_reports_link_and_leaves_child_running() {
    let (_root, runtime, _repo_root, _global_root) = test_runtime();
    let run_id = running_wait_fixture(&runtime, "task_pr_pipeline");
    let clock = FastPipelineWaitClock::new(&runtime, &run_id, None);

    let waited = runtime
        .wait_pipeline_runs_with_clock(
            std::slice::from_ref(&run_id),
            90,
            30,
            Some("fast-clock-test"),
            &clock,
        )
        .expect("configured timeout is a wait result");

    assert_eq!(clock.elapsed(), Duration::from_secs(90));
    assert_eq!(waited.results[0].run_id, run_id);
    assert_eq!(waited.results[0].status, "timeout");
    assert_eq!(
        runtime
            .show_job_run(&run_id)
            .expect("inspect linked child")
            .state,
        JobRunState::Running,
        "the wait deadline must not terminalize the child run"
    );
}

#[test]
fn pipeline_wait_reconciles_once_per_poll_tick_not_once_per_awaited_run() {
    use crate::application::job::run::reconcile_pass_counter;

    let (_root, runtime, _repo_root, _global_root) = test_runtime();
    let timeout_seconds = 2;
    let poll_seconds = 1;

    let one = running_wait_fixture(&runtime, "qa_wait_pass_one");
    let clock = FastPipelineWaitClock::new(&runtime, &one, None);
    let one_passes = reconcile_pass_counter::track(&runtime);
    runtime
        .wait_pipeline_runs_with_clock(
            std::slice::from_ref(&one),
            timeout_seconds,
            poll_seconds,
            Some("wait-pass-count"),
            &clock,
        )
        .expect("wait one run");
    let passes_for_one = one_passes.passes();
    drop(one_passes);

    let many: Vec<String> = (0..3)
        .map(|index| running_wait_fixture(&runtime, &format!("qa_wait_pass_many_{index}")))
        .collect();
    let clock = FastPipelineWaitClock::new(&runtime, &many[0], None);
    let many_passes = reconcile_pass_counter::track(&runtime);
    runtime
        .wait_pipeline_runs_with_clock(
            &many,
            timeout_seconds,
            poll_seconds,
            Some("wait-pass-count"),
            &clock,
        )
        .expect("wait many runs");

    assert_eq!(
        passes_for_one, 3,
        "timeout=2s poll=1s must reconcile once per loop tick (initial + two sleeps + timeout tick)"
    );
    assert_eq!(
        many_passes.passes(),
        passes_for_one,
        "reconcile passes must follow poll ticks, not awaited-run count"
    );
}

#[test]
fn pipeline_wait_keeps_a_bounded_generic_default_and_validates_the_ceiling() {
    assert_eq!(
        OrbitRuntime::normalize_pipeline_wait_timeout(None).expect("default wait timeout"),
        3_600
    );
    assert_eq!(
        OrbitRuntime::normalize_pipeline_wait_timeout(Some(PIPELINE_WAIT_MAX_TIMEOUT_SECONDS))
            .expect("maximum wait timeout"),
        PIPELINE_WAIT_MAX_TIMEOUT_SECONDS
    );
    let error =
        OrbitRuntime::normalize_pipeline_wait_timeout(Some(PIPELINE_WAIT_MAX_TIMEOUT_SECONDS + 1))
            .expect_err("timeout beyond the finite ceiling must be refused");
    assert!(
        error
            .to_string()
            .contains(&PIPELINE_WAIT_MAX_TIMEOUT_SECONDS.to_string()),
        "{error}"
    );
}

/// [ORB-12255] A routine fire records trigger provenance on the run document
/// and uses `routine:<name>` on `run.started`.
#[test]
fn routine_submit_records_trigger_and_audit_prefix() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    let yaml_path = jobs_dir.join("qa_routine_trigger.yaml");
    write_job(&yaml_path, "qa_routine_trigger", "sleep");

    let slot = "2026-09-12T03:00:00Z";
    let input = json!({ "seconds": 0 });
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "qa_routine_trigger",
            1,
            Utc::now(),
            Some(input.clone()),
            None,
        )
        .expect("insert routine run");
    runtime
        .seed_v2_pipeline_run(
            &run,
            &input,
            None,
            orbit_types::workflow::JobRunTrigger::routine("auto-task-scheduler-nebula", slot),
        )
        .expect("seed routine run");

    let state = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("pipeline state");
    let trigger = state.trigger.expect("trigger recorded");
    assert_eq!(
        trigger.kind,
        orbit_types::workflow::JobRunTriggerKind::Routine
    );
    assert_eq!(
        trigger.routine.as_deref(),
        Some("auto-task-scheduler-nebula")
    );
    assert_eq!(trigger.slot.as_deref(), Some(slot));

    runtime
        .execute_pipeline_run_worker(&run.run_id)
        .expect("worker executes");
    let event: serde_json::Value = serde_json::from_str(
        &v2_events(&runtime, &run.run_id, "run.started")
            .first()
            .expect("run.started audit event")
            .payload_json,
    )
    .expect("parse run.started");
    assert_eq!(
        event.get("job_name").and_then(serde_json::Value::as_str),
        Some("routine:auto-task-scheduler-nebula")
    );
}

#[cfg(unix)]
#[test]
fn v2_cli_agent_loop_persists_invocation_metrics() {
    let (_root, runtime, repo_root, _global_root) = test_runtime();
    let fake_bin = repo_root.join("codex");
    write_fake_codex(&fake_bin);

    let now = Utc::now();
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: "codex".to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some(fake_bin.display().to_string()),
            args: Vec::new(),
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            env: HashMap::new(),
            sandbox: None,
            allow_fallback: false,
            created_at: Some(now),
            updated_at: Some(now),
        })
        .expect("seed fake codex executor");

    let yaml_path = repo_root.join("qa_cli_metrics.yaml");
    write_cli_metrics_job(
        &yaml_path,
        "qa_cli_metrics",
        "codex_metrics",
        "codex",
        "gpt-test",
    );
    let task = runtime
        .add_task(TaskAddParams {
            title: "Metrics fixture".to_string(),
            description: "Task fixture for CLI invocation metrics.".to_string(),
            ..Default::default()
        })
        .expect("seed task for CLI envelope");

    let result = runtime
        .run_job_v2_from_yaml(
            &yaml_path,
            json!({
                "prompt": "collect metrics",
                "task_id": task.id.clone(),
                "crew": "sol"
            }),
        )
        .expect("cli metrics job succeeds");

    let records = runtime
        .invocation_records(InvocationQuery {
            job_run_id: Some(result.run_id.clone()),
            limit: 10,
            ..InvocationQuery::default()
        })
        .expect("query invocation records");
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.activity_id, "codex_metrics");
    assert_eq!(record.agent, "codex");
    assert_eq!(
        record.model.as_deref(),
        Some(orbit_common::model_defaults::CODEX_SOL_MODEL)
    );
    assert_eq!(record.input_tokens, 100);
    assert_eq!(record.cache_read_tokens, 25);
    assert_eq!(record.output_tokens, 12);
    assert_eq!(record.task_ids, vec![task.id]);
    assert_eq!(record.tool_call_count, 1);
    assert_eq!(record.tool_calls[0].tool_name, "command_execution");

    let activity = runtime
        .activity_invocation_metrics()
        .expect("activity metrics");
    assert!(activity.iter().any(|row| {
        row.activity_id == "codex_metrics"
            && row.agent == "codex"
            && row.model.as_deref() == Some(orbit_common::model_defaults::CODEX_SOL_MODEL)
            && row.total_input_tokens == 100
            && row.total_output_tokens == 12
            && row.total_tool_calls == 1
    }));

    let tools = runtime.tool_invocation_metrics().expect("tool metrics");
    assert!(tools.iter().any(|row| {
        row.activity_id == "codex_metrics"
            && row.tool_name == "command_execution"
            && row.call_count == 1
    }));
}

#[cfg(unix)]
#[test]
fn v2_claude_fable_alias_persists_provider_reported_model_and_cost() {
    let (_root, runtime, repo_root, _global_root) = test_runtime();
    let fake_bin = repo_root.join("claude");
    write_fake_cli_response(
        &fake_bin,
        r#"{"type":"result","subtype":"success","result":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}","total_cost_usd":0.286169,"modelUsage":{"claude-haiku-4-5-20251001":{"costUSD":0.000598,"canonicalModel":"claude-haiku-4-5"},"claude-fable-5":{"costUSD":0.285571,"canonicalModel":"claude-fable-5"}},"usage":{"input_tokens":2,"output_tokens":102}}"#,
    );

    let now = Utc::now();
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: "claude".to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some(fake_bin.display().to_string()),
            args: Vec::new(),
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            env: HashMap::new(),
            sandbox: None,
            allow_fallback: false,
            created_at: Some(now),
            updated_at: Some(now),
        })
        .expect("seed fake Claude executor");

    let yaml_path = repo_root.join("qa_fable_metrics.yaml");
    write_cli_metrics_job(
        &yaml_path,
        "qa_fable_metrics",
        "fable_metrics",
        "claude",
        "fable",
    );
    let result = runtime
        .run_job_v2_from_yaml(&yaml_path, json!({"prompt": "collect metrics"}))
        .expect("Claude fable metrics job succeeds");

    let records = runtime
        .invocation_records(InvocationQuery {
            job_run_id: Some(result.run_id),
            limit: 1,
            ..InvocationQuery::default()
        })
        .expect("query invocation records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].activity_id, "fable_metrics");
    assert_eq!(records[0].agent, "claude");
    assert_eq!(records[0].model.as_deref(), Some("claude-fable-5"));
    assert_eq!(records[0].provider_cost_usd, Some(0.286169));
}

fn write_three_step_job(path: &Path, name: &str) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  steps:
    - id: nap0
      spec:
        type: deterministic
        action: sleep
        config: {{}}
    - id: nap1
      spec:
        type: deterministic
        action: sleep
        config: {{}}
    - id: nap2
      spec:
        type: deterministic
        action: sleep
        config: {{}}
"#
    );
    std::fs::write(path, yaml).expect("write three-step job yaml");
}

/// [ORB-10002] Host checkpoint persistence: `checkpoint_step` records the
/// step into the run's `PipelineState` (state, output, cursor) and merges
/// the output into `pipeline[step_id]`; the payload is one step's output.
#[test]
fn checkpoint_step_records_into_run_state() {
    let (_root, runtime, _repo_root, _global_root) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("qa_ckpt", 1, Utc::now(), Some(json!({"seconds": 0})), None)
        .expect("insert run");
    let initial = orbit_types::workflow::PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        json!({"seconds": 0}),
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &initial)
        .expect("write initial state");

    <OrbitRuntime as RuntimeHost>::checkpoint_step(
        &runtime,
        &run.run_id,
        0,
        "nap0",
        &json!({"ok": 0}),
    )
    .expect("checkpoint step 0");
    <OrbitRuntime as RuntimeHost>::checkpoint_step(
        &runtime,
        &run.run_id,
        1,
        "nap1",
        &json!({"ok": 1}),
    )
    .expect("checkpoint step 1");

    let state = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    assert_eq!(
        state.step_states.get(&0),
        Some(&orbit_types::workflow::JobRunState::Success)
    );
    assert_eq!(
        state.step_states.get(&1),
        Some(&orbit_types::workflow::JobRunState::Success)
    );
    assert_eq!(state.step_outputs.get(&0), Some(&json!({"ok": 0})));
    assert_eq!(state.next_step_index, 2);
    assert_eq!(state.pipeline.get("nap0"), Some(&json!({"ok": 0})));
    assert_eq!(state.pipeline.get("nap1"), Some(&json!({"ok": 1})));
    assert_eq!(
        state.pipeline.get("seconds"),
        Some(&json!(0)),
        "checkpoints merge by step id and keep the seeded input keys"
    );
}

/// [ORB-10002] A checkpoint against a run that was never persisted is a
/// silent no-op (direct `execute_job` callers without a run row).
#[test]
fn checkpoint_step_without_run_row_is_noop() {
    let (_root, runtime, _repo_root, _global_root) = test_runtime();
    <OrbitRuntime as RuntimeHost>::checkpoint_step(
        &runtime,
        "jrun-never-persisted",
        0,
        "nap0",
        &json!({}),
    )
    .expect("no-op checkpoint");
}

/// [ORB-10002] Acceptance-shaped test: a run is "SIGKILLed" between steps
/// (a real child worker process is killed, the run row stays `running`,
/// step 0's checkpoint is already persisted), then a fresh scan marks it
/// `interrupted` and `resume_job_run` completes only the remaining steps.
///
/// The interruption itself is state-simulated (checkpoint written through
/// the real host path, run left running against a real dead pid) rather
/// than SIGKILLing a live orbit process mid-run; the orphan-liveness and
/// resume paths exercised are the production ones.
#[cfg(unix)]
#[test]
fn interrupted_run_resumes_skipping_checkpointed_steps() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_three_step_job(&jobs_dir.join("qa_resume_ckpt.yaml"), "qa_resume_ckpt");

    // Simulate the interrupted first attempt: a real worker process owns
    // the run, step 0's checkpoint lands, then the worker dies hard.
    let input = json!({"seconds": 0});
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("qa_resume_ckpt", 1, Utc::now(), Some(input.clone()), None)
        .expect("insert run");
    let initial = orbit_types::workflow::PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        input.clone(),
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &initial)
        .expect("write initial state");
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn fake worker");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), child.id())
        .expect("mark running under fake worker pid");
    <OrbitRuntime as RuntimeHost>::checkpoint_step(
        &runtime,
        &run.run_id,
        0,
        "nap0",
        &json!({"checkpointed": true}),
    )
    .expect("persist step 0 checkpoint");
    child.kill().expect("SIGKILL fake worker");
    child.wait().expect("reap fake worker");

    // Orphan scan (also runs at workspace open / every run query): the
    // dead owner flips the stuck `running` run to `interrupted`.
    let shown = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(shown.state, JobRunState::Interrupted);
    assert!(shown.steps.iter().any(|step| {
        step.state == JobRunState::Interrupted
            && step.error_message.as_deref().is_some_and(|message| {
                message.contains("recorded worker process is no longer alive")
            })
    }));

    // Resume: a new linked run completes only the remaining steps.
    let result = runtime
        .resume_job_run(&run.run_id)
        .expect("resume interrupted run");
    assert!(result.success);
    assert_ne!(result.run_id, run.run_id);
    assert_eq!(
        result.pipeline.get("nap0"),
        Some(&json!({"checkpointed": true})),
        "checkpointed step 0 output must be fed into the resumed pipeline"
    );
    assert!(result.pipeline.get("nap1").is_some());
    assert!(result.pipeline.get("nap2").is_some());

    let resumed = runtime.show_job_run(&result.run_id).expect("show resumed");
    assert_eq!(resumed.state, JobRunState::Success);
    assert_eq!(resumed.attempt, 2);
    assert_eq!(
        resumed.retry_source_run_id.as_deref(),
        Some(run.run_id.as_str())
    );
    let resumed_state = runtime
        .read_run_state(&result.run_id)
        .expect("read resumed checkpoint")
        .expect("resumed checkpoint exists");
    assert_eq!(resumed_state.step_states.len(), 3);
    assert_eq!(resumed_state.step_outputs.len(), 3);
    assert_eq!(
        resumed_state.pipeline.get("nap0"),
        Some(&json!({"checkpointed": true}))
    );
    assert!(resumed_state.pipeline.get("nap1").is_some());
    assert!(resumed_state.pipeline.get("nap2").is_some());

    // Step 0 was NOT re-executed: it is audited as skipped-for-resume and
    // never started; steps 1 and 2 started for real.
    let skipped = v2_events(&runtime, &result.run_id, "step.skipped");
    assert!(skipped.iter().any(|row| {
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).expect("payload");
        payload["step_id"] == json!("nap0")
            && payload["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("resume"))
    }));
    let started_ids: Vec<String> = v2_events(&runtime, &result.run_id, "step.started")
        .iter()
        .map(|row| {
            let payload: serde_json::Value =
                serde_json::from_str(&row.payload_json).expect("payload");
            payload["step_id"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    assert!(!started_ids.contains(&"nap0".to_string()));
    assert!(started_ids.contains(&"nap1".to_string()));
    assert!(started_ids.contains(&"nap2".to_string()));

    // Source run stays interrupted; resume never mutates its history.
    let source_after = runtime.show_job_run(&run.run_id).expect("show source");
    assert_eq!(source_after.state, JobRunState::Interrupted);
    let source_state_after = runtime
        .read_run_state(&run.run_id)
        .expect("read source checkpoint")
        .expect("source checkpoint exists");
    assert_eq!(source_state_after.step_states.len(), 1);
    assert!(source_state_after.pipeline.get("nap1").is_none());
    assert!(source_state_after.pipeline.get("nap2").is_none());
}

/// [ORB-10002] Resume refuses runs that are not interrupted / failed /
/// timed-out.
#[test]
fn resume_rejects_successful_runs() {
    let (_root, runtime, repo_root, _global_root) = test_runtime();
    let yaml_path = repo_root.join("qa_resume_guard.yaml");
    write_job(&yaml_path, "qa_resume_guard", "sleep");
    let result = runtime
        .run_job_v2_from_yaml(&yaml_path, json!({"seconds": 0}))
        .expect("run succeeds");

    let error = runtime
        .resume_job_run(&result.run_id)
        .expect_err("resume of a successful run must fail");
    assert!(
        error
            .to_string()
            .contains("resume requires an interrupted, failed, or timed-out run"),
        "{error}"
    );
}

/// [ORB-10385] An unregistered action is now caught by validation before
/// the first step dispatches rather than by the dispatcher, so the
/// diagnostic names the activity and the runtime skew. The durable
/// bookkeeping this test guards — `Failed` run state, a persisted step
/// error, and an `error` run.finished audit event — is unchanged.
#[test]
fn failing_direct_run_records_failure_state() {
    let (_root, runtime, repo_root, _global_root) = test_runtime();
    let yaml_path = repo_root.join("qa_failing.yaml");
    write_job(&yaml_path, "qa_failing", "missing_action");

    let err = runtime
        .run_job_v2_from_yaml(&yaml_path, json!({}))
        .expect_err("direct job run should fail");
    assert!(
        err.to_string().contains("missing_action") && err.to_string().contains("not registered"),
        "{err}"
    );

    let history = runtime.job_history("qa_failing").expect("failure history");
    let run = history.first().expect("failed run");
    assert_eq!(run.state, JobRunState::Failed);
    assert!(run.steps.iter().any(|step| {
        step.error_message
            .as_deref()
            .is_some_and(|message| message.contains("not registered"))
    }));
    let event: serde_json::Value = serde_json::from_str(
        &v2_events(&runtime, &run.run_id, "run.finished")
            .first()
            .expect("run_finished audit event")
            .payload_json,
    )
    .expect("parse run_finished");
    assert_eq!(
        event.get("outcome").and_then(serde_json::Value::as_str),
        Some("error")
    );
    assert!(
        event
            .get("error_message")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.contains("not registered"))
    );
    assert!(
        runtime
            .read_run_state(&run.run_id)
            .expect("read run state")
            .is_some()
    );
}
