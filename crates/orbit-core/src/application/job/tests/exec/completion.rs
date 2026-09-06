use std::sync::{Arc, Mutex};

use orbit_engine::{DispatchError, ResolvedCliExecutor, RuntimeHost};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::task::{ExternalRef, Task, TaskPriority, TaskStatus};
use orbit_types::workflow::JobV2StepBody;
use serde_json::{Value, json};

use super::{resolved_job, seed_default_catalogs, test_runtime, try_execute_job};
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

/// Exercises the actual job executor's terminal-failure routing from
/// `complete_pr` into `pr_failure_handoff`, while the same host boundary
/// scripts the private VCS status/capability/merge calls. This is deliberately
/// broader than a direct `pr_complete` unit test: the pipeline must preserve
/// the published candidate after the real merge step fails.
#[test]
fn completion_merge_failure_routes_to_review_recovery_without_republishing() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let run_id = "jrun-completion-merge-failure";
    let task = runtime
        .add_task(TaskAddParams {
            title: "Published completion candidate".to_string(),
            description: "Fixture already published and promoted to review.".to_string(),
            acceptance_criteria: vec!["Completion retry remains safe.".to_string()],
            plan: "Exercise completion failure recovery.".to_string(),
            context_files: vec!["src/lib.rs".to_string()],
            workspace_path: Some(".".to_string()),
            status: Some(TaskStatus::Review),
            external_refs: vec![
                ExternalRef::try_new(
                    "github-pr".to_string(),
                    "42".to_string(),
                    Some("https://github.com/example/orbit/pull/42".to_string()),
                )
                .expect("github PR ref"),
            ],
            ..Default::default()
        })
        .expect("seed published task");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                job_run_id: Some(Some(run_id.to_string())),
                execution_summary: Some(
                    "Outcome: success\n\nChanges:\n- Published candidate is ready.".to_string(),
                ),
                ..Default::default()
            },
        )
        .expect("attach task to run");

    let mut job = resolved_job(&runtime, "task_pr_pipeline");
    job.steps.retain(|step| step.id == "complete_pr");
    let complete = job.steps.first_mut().expect("complete_pr step");
    complete.when = None;
    complete.recovery_activity = None;
    complete.resolved_recovery_activity = None;
    let JobV2StepBody::Target(complete) = &mut complete.body else {
        panic!("resolved complete_pr target");
    };
    complete.default_input = Some(json!({
        "job_run_id": run_id,
        "completed_task_ids": [task.id],
        "workspace_path": repo_root,
        "pr_number": "42",
        "poll_interval_seconds": 0,
        "max_wait_seconds": 0,
    }));
    let host = CompletionFailureHost::new(&runtime);

    let error = try_execute_job(
        &runtime,
        &repo_root,
        &host,
        job,
        json!({ "task_ids": [task.id] }),
        run_id,
    )
    .expect_err("private merge denial must fail the pipeline");

    let message = error.to_string();
    assert!(
        message.contains("could not request rebase merge"),
        "{message}"
    );
    assert!(message.contains("merge permission denied"), "{message}");
    let persisted = runtime.get_task(&task.id).expect("preserved task");
    assert_eq!(persisted.status, TaskStatus::Review);
    assert_eq!(persisted.github_pr_number(), Some("42"));
    assert_eq!(
        persisted
            .external_refs
            .iter()
            .find(|reference| reference.system == "github-pr")
            .and_then(|reference| reference.url.as_deref()),
        Some("https://github.com/example/orbit/pull/42")
    );
    assert_eq!(
        host.operations(),
        vec!["pr.status", "pr.merge_capabilities", "pr.merge"],
        "completion must read status and policy before the denied merge"
    );
    let comments = runtime.get_task_comments(&task.id).expect("task comments");
    let recovery = comments.last().expect("completion recovery comment");
    assert!(recovery.message.contains("preserved PR #42"));
    assert!(!recovery.message.contains("Manual resolution required"));
}

struct CompletionFailureHost<'a> {
    runtime: &'a OrbitRuntime,
    operations: Mutex<Vec<String>>,
}

impl<'a> CompletionFailureHost<'a> {
    fn new(runtime: &'a OrbitRuntime) -> Self {
        Self {
            runtime,
            operations: Mutex::new(Vec::new()),
        }
    }

    fn operations(&self) -> Vec<String> {
        self.operations.lock().expect("operations lock").clone()
    }
}

impl RuntimeHost for CompletionFailureHost<'_> {
    fn get_task(&self, task_id: &str) -> Result<Task, orbit_common::OrbitError> {
        self.runtime.get_task(task_id)
    }

    fn get_task_comments(
        &self,
        task_id: &str,
    ) -> Result<Vec<orbit_types::task::TaskComment>, orbit_common::OrbitError> {
        self.runtime.get_task_comments(task_id)
    }

    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, orbit_common::OrbitError> {
        RuntimeHost::list_tasks_filtered(
            self.runtime,
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: orbit_engine::TaskAutomationUpdate,
    ) -> Result<(), orbit_common::OrbitError> {
        RuntimeHost::apply_task_automation_update(self.runtime, task_id, update)
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        _input: Value,
    ) -> Result<Value, orbit_common::OrbitError> {
        self.operations
            .lock()
            .expect("operations lock")
            .push(operation.to_string());
        match operation {
            "pr.status" => Ok(json!({
                "pull_request": {
                    "number": 42,
                    "state": "OPEN",
                    "mergedAt": Value::Null,
                    "mergeStateStatus": "CLEAN",
                }
            })),
            "pr.merge_capabilities" => Ok(json!({
                "repository": {
                    "name_with_owner": "example/orbit",
                    "base_branch": "agent-main",
                    "allow_squash_merge": false,
                    "allow_rebase_merge": true,
                    "allow_merge_commit": true,
                    "allow_auto_merge": true,
                    "requires_linear_history": true,
                }
            })),
            "pr.merge" => Err(orbit_common::OrbitError::Execution(
                "gh: merge permission denied".to_string(),
            )),
            other => Err(orbit_common::OrbitError::Execution(format!(
                "unexpected private VCS operation {other}"
            ))),
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
