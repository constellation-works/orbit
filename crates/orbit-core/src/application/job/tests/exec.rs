use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

mod already_landed;
mod ci_sweep;
mod completion;
mod epic_review_gate;
mod local_ship;
mod review_gate;

use chrono::Utc;
use orbit_engine::{
    DispatchError, JobOutcome, ResolvedCliExecutor, RuntimeHost, TaskActivityUpdate, V2AuditWriter,
    execute_job_with_resume, resolve_job_catalog_refs_for_execution,
};
use orbit_store::{InvocationQuery, TaskReservationReleaseReason, V2AuditEventFilter};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    ActivityV2Spec, ExecutorDef, ExecutorType, JobRunState, JobV2Step, JobV2StepBody,
};
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::SYSTEM_AUDIT_IDENTITY;
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

fn epic_assemble_steps(
    job: &orbit_types::workflow::activity_job::JobV2,
) -> &[orbit_types::workflow::activity_job::JobV2Step] {
    let assemble = job
        .steps
        .iter()
        .find(|step| step.id == "assemble")
        .expect("epic assemble step");
    let JobV2StepBody::Loop { loop_ } = &assemble.body else {
        panic!("epic assemble step");
    };
    &loop_.steps
}

fn patch_epic_child_run_input(drain: &mut orbit_types::workflow::activity_job::JobV2Step) {
    let JobV2StepBody::Loop { loop_ } = &mut drain.body else {
        panic!("epic drain step");
    };
    let JobV2StepBody::Target(land_child) = &mut loop_.steps[0].body else {
        panic!("resolved epic child step");
    };
    let run_input = land_child
        .default_input
        .as_mut()
        .and_then(|value| value.get_mut("run_input"))
        .and_then(Value::as_object_mut)
        .expect("epic child run input");
    run_input.insert(
        "base_branch".to_string(),
        Value::String("epic/ORB-EPIC".to_string()),
    );
    run_input.insert(
        "landing_branch".to_string(),
        Value::String("agent-main".to_string()),
    );
}

fn stub_epic_finisher(global_root: &Path) {
    std::fs::write(
        global_root.join("resources/activities/epic_orchestrator.yaml"),
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: epic_orchestrator
spec:
  type: deterministic
  description: Test stub for the epic worktree finisher.
  input_schema_json:
    type: object
  output_schema_json:
    type: object
  action: test_epic_finish
  config: {}
"#,
    )
    .expect("stub epic finisher activity");
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

fn init_git_repo(path: &Path) {
    git_in(path, &["init"]);
    git_in(path, &["config", "user.name", "Orbit Test"]);
    git_in(
        path,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    std::fs::write(path.join("README.md"), "base\n").expect("write initial file");
    git_in(path, &["add", "README.md"]);
    git_in(path, &["commit", "-m", "initial"]);
    git_in(path, &["checkout", "-b", "epic/ORB-EMPTY-EPIC"]);
}

fn git_subject(path: &Path) -> String {
    let output = Command::new("git")
        .current_dir(path)
        .args(["log", "-1", "--format=%s"])
        .output()
        .expect("git log");
    assert!(
        output.status.success(),
        "git log failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git subject utf8")
        .trim()
        .to_string()
}

fn try_execute_epic_job(
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
            .map_err(|error| DispatchError::JobExecution(format!("open audit store: {error}")))?,
        runtime.workspace_id().map_err(|error| {
            DispatchError::JobExecution(format!("resolve workspace id: {error}"))
        })?,
        run_id,
        SYSTEM_AUDIT_IDENTITY,
        Some(repo_root),
    )
    .expect("audit writer");
    execute_job_with_resume(&job, input, run_id, writer, host, None)
}

fn try_execute_epic_drain_job(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    host: &dyn RuntimeHost,
    input: Value,
    run_id: &str,
) -> Result<JobOutcome, DispatchError> {
    let mut job = resolved_job(runtime, "epic_pipeline");
    let assemble_steps = epic_assemble_steps(&job);
    let descendants = assemble_steps
        .iter()
        .find(|step| step.id == "descendants")
        .cloned()
        .expect("descendants");
    let mut drain = assemble_steps
        .iter()
        .find(|step| step.id == "drain")
        .cloned()
        .expect("drain");
    patch_epic_child_run_input(&mut drain);
    job.steps = vec![descendants, drain];
    try_execute_epic_job(runtime, repo_root, host, job, input, run_id)
}

fn patch_assemble_for_scripted_host(
    assemble: &mut orbit_types::workflow::activity_job::JobV2Step,
    workspace: &Path,
) {
    let JobV2StepBody::Loop { loop_ } = &mut assemble.body else {
        panic!("epic assemble step");
    };
    loop_.steps.retain(|step| step.id != "commit_finish");
    for step in &mut loop_.steps {
        match step.id.as_str() {
            "drain" => patch_epic_child_run_input(step),
            "finish" => {
                let JobV2StepBody::Target(finish) = &mut step.body else {
                    panic!("resolved epic finish step");
                };
                let input = finish
                    .default_input
                    .as_mut()
                    .and_then(Value::as_object_mut)
                    .expect("finish input");
                let workspace = workspace.display().to_string();
                input.insert(
                    "workspace_path".to_string(),
                    Value::String(workspace.clone()),
                );
                input.insert("repo_root".to_string(), Value::String(workspace));
            }
            _ => {}
        }
    }
}

fn try_execute_epic_assemble_job(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    host: &dyn RuntimeHost,
    input: Value,
    run_id: &str,
) -> Result<JobOutcome, DispatchError> {
    let mut job = resolved_job(runtime, "epic_pipeline");
    let mut assemble = job
        .steps
        .iter()
        .find(|step| step.id == "assemble")
        .cloned()
        .expect("assemble");
    patch_assemble_for_scripted_host(&mut assemble, repo_root);
    job.steps = vec![assemble];
    try_execute_epic_job(runtime, repo_root, host, job, input, run_id)
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

struct ScriptedEpicHost<'a> {
    runtime: &'a OrbitRuntime,
    descendant_ids: Mutex<Vec<String>>,
    finish_authored_child: Option<String>,
    commit_on_finish: bool,
    land_children: bool,
    no_diff: bool,
    ship_mode: &'static str,
    calls: Mutex<Vec<(String, Value)>>,
}

impl<'a> ScriptedEpicHost<'a> {
    fn new(runtime: &'a OrbitRuntime, descendant_ids: Vec<String>) -> Self {
        Self {
            runtime,
            descendant_ids: Mutex::new(descendant_ids),
            finish_authored_child: None,
            commit_on_finish: false,
            land_children: true,
            no_diff: false,
            ship_mode: "pr",
            calls: Mutex::new(Vec::new()),
        }
    }

    fn author_child_on_first_finish(mut self, child_id: impl Into<String>) -> Self {
        self.finish_authored_child = Some(child_id.into());
        self
    }

    fn commit_on_finish(mut self) -> Self {
        self.commit_on_finish = true;
        self
    }

    fn local_mode(mut self) -> Self {
        self.ship_mode = "local";
        self
    }

    fn no_diff(mut self) -> Self {
        self.no_diff = true;
        self
    }

    fn never_land_children(mut self) -> Self {
        self.land_children = false;
        self
    }

    fn inputs_for(&self, action: &str) -> Vec<Value> {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .filter(|(recorded, _)| recorded == action)
            .map(|(_, input)| input.clone())
            .collect()
    }

    fn current_descendants(&self) -> Vec<String> {
        self.descendant_ids.lock().expect("descendant ids").clone()
    }
}

impl RuntimeHost for ScriptedEpicHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        let recorded = action.strip_prefix("scripted_").unwrap_or(action);
        self.calls
            .lock()
            .expect("call log")
            .push((recorded.to_string(), input.clone()));
        match recorded {
            "resolve_workspace_ship_input" => Ok(json!({
                "mode": self.ship_mode,
                "base_branch": "agent-main",
            })),
            "worktree_setup" => {
                let job_run_id = input
                    .get("job_run_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("epic-ORB-EPIC");
                Ok(json!({
                    "job_run_id": job_run_id,
                    "batch_id": job_run_id,
                    "workspace_path": self.runtime.paths().repo_root,
                    "head_ref": "epic/ORB-EPIC",
                    "base_ref": "origin/agent-main",
                    "base_sha": "1111111111111111111111111111111111111111",
                }))
            }
            "list_epic_descendants" => {
                let descendant_ids = self.current_descendants();
                let empty = descendant_ids.is_empty();
                let fail_if_nonempty = input
                    .get("fail_if_nonempty")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if fail_if_nonempty && !empty {
                    return Err(DispatchError::DeterministicActionFailed {
                        action: action.to_string(),
                        message: format!(
                            "epic descendants remain after drain: tasks=[{}]",
                            descendant_ids.join(", ")
                        ),
                    });
                }
                Ok(json!({
                    "epic_task_id": input
                        .get("epic_task_id")
                        .and_then(Value::as_str)
                        .unwrap_or("ORB-EPIC"),
                    "task_count": descendant_ids.len(),
                    "task_ids": descendant_ids,
                    "empty": empty,
                }))
            }
            "invoke_and_wait" => {
                if self.land_children
                    && let Some(task_ids) = input
                        .get("run_input")
                        .and_then(|value| value.get("task_ids"))
                        .and_then(Value::as_array)
                {
                    self.descendant_ids
                        .lock()
                        .expect("descendant ids")
                        .retain(|id| {
                            !task_ids
                                .iter()
                                .any(|value| value.as_str() == Some(id.as_str()))
                        });
                }
                Ok(json!({
                    "run_id": "jrun-scripted-epic-child",
                    "status": "succeeded",
                }))
            }
            "test_epic_finish" => {
                let finish_count = self.inputs_for("test_epic_finish").len();
                if finish_count == 1
                    && let Some(child_id) = &self.finish_authored_child
                {
                    self.descendant_ids
                        .lock()
                        .expect("descendant ids")
                        .push(child_id.clone());
                }
                if self.commit_on_finish {
                    let workspace = input
                        .get("workspace_path")
                        .and_then(Value::as_str)
                        .expect("finisher workspace_path");
                    let workspace = Path::new(workspace);
                    std::fs::write(workspace.join("finisher.txt"), "finisher work\n")
                        .expect("write finisher file");
                    git_in(workspace, &["add", "finisher.txt"]);
                    git_in(
                        workspace,
                        &["commit", "-m", "finisher: close remaining epic work"],
                    );
                }
                Ok(json!({ "summary": "finished" }))
            }
            "git_commit" => Ok(json!({
                "phase": "commit",
                "decision": if self.no_diff {
                    "skipped_no_diff_expected"
                } else {
                    "already_committed"
                },
                "committed": false,
                "skipped_no_diff_expected": self.no_diff,
            })),
            "pr_prepare" => Ok(json!({
                "phase": "prepare",
                "decision": "already_fresh",
                "head": "epic/ORB-EPIC",
                "head_sha": "2222222222222222222222222222222222222222",
                "base": "agent-main",
                "base_ref": "origin/agent-main",
                "base_sha": "1111111111111111111111111111111111111111",
                "remote_sha": Value::Null,
                "commits_behind": 0,
                "commits_ahead": 1,
                "sync_required": false,
            })),
            "git_rebase" => Ok(json!({
                "phase": "rebase",
                "decision": "skipped_current",
                "head": "epic/ORB-EPIC",
                "head_sha": "2222222222222222222222222222222222222222",
                "head_sha_before": "2222222222222222222222222222222222222222",
                "base": "agent-main",
                "base_ref": "origin/agent-main",
                "base_sha": "1111111111111111111111111111111111111111",
                "remote_sha_before": Value::Null,
                "rewritten": false,
            })),
            "git_push" => Ok(json!({
                "phase": "push",
                "decision": "performed",
                "branch": "epic/ORB-EPIC",
                "local_sha": "2222222222222222222222222222222222222222",
                "force_with_lease": false,
            })),
            "pr_open" => Ok(json!({
                "phase": "open",
                "decision": "created",
                "pr_created": true,
                "pr_reused": false,
                "pr_number": "42",
                "pr_url": "https://example.test/pr/42",
            })),
            "pr_promote" => Ok(json!({
                "phase": "promote",
                "decision": "performed",
                "performed_task_ids": ["ORB-EPIC"],
                "reused_task_ids": [],
            })),
            "git_merge" => Ok(json!({
                "base": "agent-main",
                "workspace_path": self.runtime.paths().repo_root,
                "workspace_branch": "epic/ORB-EPIC",
            })),
            "update_task" => Ok(json!({
                "task_id": input.get("task_id"),
                "status": input.get("status"),
            })),
            // [ORB-11333] The gate reads the run's captured review admission;
            // these fixtures persist none, so it reports `applies: false`.
            "pipeline_success_guard" | "review_gate_admit" | "review_gate_settle" => {
                <OrbitRuntime as RuntimeHost>::run_deterministic(
                    self.runtime,
                    action,
                    config,
                    input,
                    tool_context,
                )
            }
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

    // `task_complete` runs as the real production activity (it is never
    // retargeted to a scripted action), so it needs a real task to read and
    // mutate rather than a scripted echo.
    fn get_task(&self, task_id: &str) -> Result<Task, orbit_common::OrbitError> {
        self.runtime.get_task(task_id)
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, orbit_common::OrbitError> {
        self.runtime.update_task_from_activity(task_id, update)
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
    assert_eq!(dispatch["status"], json!("succeeded"));
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

#[test]
fn epic_pipeline_dispatches_three_children_serially_without_push_or_pr_inputs() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let child_ids = vec![
        "ORB-CHILD-1".to_string(),
        "ORB-CHILD-2".to_string(),
        "ORB-CHILD-3".to_string(),
    ];
    let host = ScriptedEpicHost::new(&runtime, child_ids.clone());

    let outcome = try_execute_epic_drain_job(
        &runtime,
        &repo_root,
        &host,
        json!({ "epic_task_id": "ORB-EPIC" }),
        "jrun-scripted-epic",
    )
    .expect("execute epic pipeline");

    assert!(outcome.success);
    let invokes = host.inputs_for("invoke_and_wait");
    assert_eq!(invokes.len(), 3);
    for (invoke, child_id) in invokes.iter().zip(child_ids) {
        assert_eq!(invoke["job_name"], "task_local_pipeline");
        assert_eq!(invoke["run_input"]["task_ids"], json!([child_id]));
        assert_eq!(invoke["run_input"]["base_branch"], "epic/ORB-EPIC");
        assert_eq!(invoke["run_input"]["base_sync"], "local");
        assert_eq!(invoke["run_input"]["auto_push"], false);
        assert_eq!(invoke["run_input"]["terminal_status"], "done");
        assert_eq!(invoke["run_input"]["landing_branch"], "agent-main");
    }
    assert!(host.inputs_for("git_push").is_empty());
    assert!(host.inputs_for("pr_open").is_empty());
}

#[test]
fn epic_pipeline_with_no_children_runs_no_child_pipeline() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let host = ScriptedEpicHost::new(&runtime, Vec::new());

    let outcome = try_execute_epic_drain_job(
        &runtime,
        &repo_root,
        &host,
        json!({ "epic_task_id": "ORB-EMPTY-EPIC" }),
        "jrun-scripted-empty-epic",
    )
    .expect("execute empty epic pipeline");

    assert!(outcome.success);
    assert!(host.inputs_for("invoke_and_wait").is_empty());
    assert!(host.inputs_for("pipeline_success_guard").is_empty());
}

#[test]
fn epic_pipeline_with_no_children_runs_finisher_and_keeps_its_commits() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    stub_epic_finisher(&global_root);
    init_git_repo(&runtime.paths().repo_root);
    let host = ScriptedEpicHost::new(&runtime, Vec::new()).commit_on_finish();

    let outcome = try_execute_epic_assemble_job(
        &runtime,
        &runtime.paths().repo_root,
        &host,
        json!({ "epic_task_id": "ORB-EMPTY-EPIC" }),
        "jrun-scripted-empty-epic-finish",
    )
    .expect("execute empty epic pipeline");

    assert!(outcome.success);
    assert!(host.inputs_for("invoke_and_wait").is_empty());
    let finishes = host.inputs_for("test_epic_finish");
    assert_eq!(finishes.len(), 1);
    assert_eq!(
        finishes[0]["workspace_path"],
        runtime.paths().repo_root.display().to_string()
    );
    assert_eq!(finishes[0]["repo_root"], finishes[0]["workspace_path"]);
    assert_eq!(
        git_subject(&runtime.paths().repo_root),
        "finisher: close remaining epic work"
    );
    assert_eq!(
        std::fs::read_to_string(runtime.paths().repo_root.join("finisher.txt"))
            .expect("read finisher file"),
        "finisher work\n"
    );
}

#[test]
fn epic_pipeline_reenters_drain_when_finisher_authors_a_child() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    stub_epic_finisher(&global_root);
    let host = ScriptedEpicHost::new(&runtime, vec!["ORB-CHILD-1".to_string()])
        .author_child_on_first_finish("ORB-AUTHORED");

    let outcome = try_execute_epic_assemble_job(
        &runtime,
        &repo_root,
        &host,
        json!({ "epic_task_id": "ORB-EPIC" }),
        "jrun-scripted-epic-reenter",
    )
    .expect("execute epic pipeline with authored child");

    assert!(outcome.success);
    let invokes = host.inputs_for("invoke_and_wait");
    assert_eq!(invokes.len(), 2);
    assert_eq!(invokes[0]["run_input"]["task_ids"], json!(["ORB-CHILD-1"]));
    assert_eq!(invokes[1]["run_input"]["task_ids"], json!(["ORB-AUTHORED"]));
    assert_eq!(host.inputs_for("test_epic_finish").len(), 2);
    assert!(host.current_descendants().is_empty());
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

fn try_execute_full_epic_job(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    host: &dyn RuntimeHost,
    input: Value,
    run_id: &str,
) -> Result<JobOutcome, DispatchError> {
    let mut job = resolved_job(runtime, "epic_pipeline");
    retarget_engine_actions_for_scripted_host(&mut job);
    try_execute_epic_job(runtime, repo_root, host, job, input, run_id)
}

#[test]
fn epic_pipeline_pr_mode_delivers_one_pr_against_the_workspace_base() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    stub_epic_finisher(&global_root);
    let host = ScriptedEpicHost::new(&runtime, vec!["ORB-CHILD-1".to_string()]);

    let outcome = try_execute_full_epic_job(
        &runtime,
        &repo_root,
        &host,
        json!({ "epic_task_id": "ORB-EPIC" }),
        "jrun-scripted-epic-pr",
    )
    .expect("execute epic pr delivery");

    assert!(outcome.success);
    assert_eq!(host.inputs_for("worktree_setup").len(), 1);
    assert_eq!(host.inputs_for("pr_open").len(), 1);
    assert_eq!(host.inputs_for("pr_promote").len(), 1);
    assert!(host.inputs_for("git_merge").is_empty());
    let prepare = &host.inputs_for("pr_prepare")[0];
    assert_eq!(prepare["base"], "agent-main");
    assert_eq!(
        prepare["workspace_path"],
        runtime.paths().repo_root.display().to_string()
    );
    assert_eq!(prepare["completed_task_ids"], json!(["ORB-EPIC"]));
    assert!(
        host.inputs_for("invoke_and_wait")
            .iter()
            .all(|input| input["job_name"] != "task_pr_pipeline"),
        "delivery must not invoke task_pr_pipeline"
    );
    assert!(host.inputs_for("update_task").is_empty());
}

#[test]
fn epic_pipeline_local_mode_merges_once_into_the_workspace_base() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    stub_epic_finisher(&global_root);
    let host = ScriptedEpicHost::new(&runtime, vec!["ORB-CHILD-1".to_string()]).local_mode();
    let run_id = Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .to_string();

    let outcome = try_execute_full_epic_job(
        &runtime,
        &repo_root,
        &host,
        json!({ "epic_task_id": "ORB-EPIC" }),
        &run_id,
    )
    .expect("execute epic local delivery");

    assert!(outcome.success);
    assert_eq!(host.inputs_for("worktree_setup").len(), 1);
    assert!(host.inputs_for("pr_open").is_empty());
    assert!(host.inputs_for("pr_promote").is_empty());
    assert_eq!(host.inputs_for("git_merge").len(), 1);
    let merge = &host.inputs_for("git_merge")[0];
    assert_eq!(merge["base"], "agent-main");
    assert_eq!(
        merge["workspace_path"],
        runtime.paths().repo_root.display().to_string()
    );
    let updates = host.inputs_for("update_task");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["task_id"], "ORB-EPIC");
    assert_eq!(updates[0]["status"], "done");
}

#[test]
fn epic_pipeline_no_diff_skips_empty_pr_and_promotes_the_root() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    stub_epic_finisher(&global_root);
    let host = ScriptedEpicHost::new(&runtime, Vec::new()).no_diff();

    let outcome = try_execute_full_epic_job(
        &runtime,
        &repo_root,
        &host,
        json!({ "epic_task_id": "ORB-EPIC" }),
        "jrun-scripted-epic-no-diff",
    )
    .expect("execute no-diff epic");

    assert!(outcome.success);
    assert!(host.inputs_for("pr_prepare").is_empty());
    assert!(host.inputs_for("pr_open").is_empty());
    assert!(host.inputs_for("git_push").is_empty());
    let updates = host.inputs_for("update_task");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["task_id"], "ORB-EPIC");
    assert_eq!(updates[0]["status"], "review");
}

/// [ORB-11214] `commit_delivery` sets `allow_empty: true` unconditionally, so
/// an ordinary untagged epic root can report `skipped_no_diff_expected: true`
/// with no `no-diff-expected` tag involved at all — unlike task_pr_pipeline,
/// where the same signal is tag-derived. A `--complete` run must still finish
/// this epic root, not fail on a tag guard the operator never asked for. This
/// drives `complete_pr_no_diff`'s real activity (never scripted) against a
/// real, untagged task to prove the completion itself succeeds, not just that
/// the pipeline steps are shaped correctly.
#[test]
fn epic_pipeline_completes_an_untagged_no_diff_root_when_authorized() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    stub_epic_finisher(&global_root);
    let epic_task_id = seed_gate_task(&runtime, &repo_root, TaskStatus::Review);
    assert!(
        runtime
            .get_task(&epic_task_id)
            .expect("seeded epic root")
            .tags
            .is_empty(),
        "the epic root under test must not carry the no-diff-expected tag"
    );
    let host = ScriptedEpicHost::new(&runtime, Vec::new()).no_diff();

    let outcome = try_execute_full_epic_job(
        &runtime,
        &repo_root,
        &host,
        json!({ "epic_task_id": epic_task_id, "completion": "done" }),
        "jrun-scripted-epic-no-diff-complete",
    )
    .expect(
        "an untagged no-diff epic root must complete under authorization, not demand the \
         no-diff-expected tag pr_complete requires",
    );

    assert!(outcome.success);
    assert!(host.inputs_for("pr_open").is_empty());
    assert!(host.inputs_for("git_push").is_empty());
    assert_eq!(
        runtime
            .get_task(&epic_task_id)
            .expect("completed epic root")
            .status,
        TaskStatus::Done
    );
}

#[test]
fn epic_pipeline_fails_closed_when_descendants_remain_and_names_them() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    stub_epic_finisher(&global_root);
    let host = ScriptedEpicHost::new(&runtime, vec!["ORB-LEFT".to_string()]).never_land_children();

    let err = try_execute_full_epic_job(
        &runtime,
        &repo_root,
        &host,
        json!({ "epic_task_id": "ORB-EPIC" }),
        "jrun-scripted-epic-leftover",
    )
    .expect_err("leftover descendants must fail closed");

    let message = err.to_string();
    assert!(message.contains("ORB-LEFT"), "{message}");
    assert!(
        message.contains("epic descendants remain after drain"),
        "{message}"
    );
    assert!(host.inputs_for("pr_open").is_empty());
    assert!(host.inputs_for("git_merge").is_empty());
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

fn seed_failed_triage_candidate(runtime: &OrbitRuntime, title: &str) -> String {
    let task = runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Fixture task blocked by a failed pipeline.".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed triage task");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .expect("insert failed pipeline run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark failed pipeline run running");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(Some(run.run_id.clone())),
                ..Default::default()
            },
        )
        .expect("couple blocked task to failed run");
    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Failed,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize failed pipeline run");
    task.id
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

#[cfg(unix)]
#[test]
fn task_triage_pipeline_applies_multiple_cli_envelope_dispositions() {
    // The triage step names `crew: system` itself, so it resolves against the
    // built-in `system` crew rather than through `workflow.system_crew`.
    let (_root, runtime, repo_root, global_root) = test_runtime_with_workspace_config(
        r#"
[workflow]
base_branch = "main"
"#,
    );
    seed_default_catalogs(&global_root);
    let environmental = seed_failed_triage_candidate(&runtime, "Environmental triage fixture");
    let code_defect = seed_failed_triage_candidate(&runtime, "Code-defect triage fixture");

    let agent_envelope = json!({
        "schemaVersion": 1,
        "status": "success",
        "result": {
            "dispositions": [
                {
                    "task_id": environmental.clone(),
                    "classification": "environmental",
                    "disposition": "rebacklog",
                    "diagnosis": "the runner lost its workspace lease",
                    "mitigation": "stale lease cleared"
                },
                {
                    "task_id": code_defect.clone(),
                    "classification": "code_defect",
                    "disposition": "stay_blocked",
                    "diagnosis": "the task still has failing tests"
                }
            ],
            "summary": "one task can retry and one needs a code fix"
        },
        "error": null
    });
    let provider_stdout = json!({
        "type": "result",
        "subtype": "success",
        "result": format!("Triage complete.\n{agent_envelope}"),
        "usage": {
            "input_tokens": 12,
            "output_tokens": 8
        }
    })
    .to_string();
    let fake_bin = repo_root.join("claude");
    write_fake_cli_response(&fake_bin, &provider_stdout);

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
        .expect("seed fake claude executor");

    let result = runtime
        .run_job_v2_from_yaml(
            &global_root.join("resources/jobs/task_triage_pipeline.yaml"),
            json!({
                "task_ids": [environmental.clone(), code_defect.clone()],
                "max_tasks": 20,
                "max_rebacklogs": 2,
            }),
        )
        .expect("task triage pipeline succeeds");

    assert!(result.success);
    assert_eq!(
        result.pipeline["triage"]["dispositions"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        result.pipeline["apply_dispositions"]["rebacklogged_count"],
        json!(1)
    );
    assert_eq!(
        result.pipeline["apply_dispositions"]["diagnosed_count"],
        json!(1)
    );
    assert_eq!(
        runtime
            .get_task(&environmental)
            .expect("environmental task")
            .status,
        TaskStatus::Backlog
    );
    assert_eq!(
        runtime
            .get_task(&code_defect)
            .expect("code defect task")
            .status,
        TaskStatus::Blocked
    );
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
/// step into the run's `PipelineState` (state, output, snapshot, cursor).
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
        &json!({"nap0": {"ok": 0}}),
    )
    .expect("checkpoint step 0");
    <OrbitRuntime as RuntimeHost>::checkpoint_step(
        &runtime,
        &run.run_id,
        1,
        "nap1",
        &json!({"ok": 1}),
        &json!({"nap0": {"ok": 0}, "nap1": {"ok": 1}}),
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
    assert_eq!(
        state.pipeline,
        json!({"nap0": {"ok": 0}, "nap1": {"ok": 1}})
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
        &json!({"nap0": {"checkpointed": true}}),
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
