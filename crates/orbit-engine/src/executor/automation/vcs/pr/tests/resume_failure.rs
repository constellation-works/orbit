use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_agent::loop_engine::audit::{AuditSink, NullSink};
use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use serde_json::{Value, json};

use super::test_support::*;
use crate::context::{RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate};
use crate::executor::automation::vcs::failure::pr_failure_handoff;
use crate::{DispatchError, V2AuditWriter, execute_job_with_resume};
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, DeterministicSpec, JobKind, JobV2, JobV2Step, JobV2StepBody,
    TargetStep,
};
use orbit_types::workflow::{JobRun, JobRunState, JobScheduleState, PipelineState};

pub(super) const TASK_ID: &str = "ORB-RESUMED-FAILURE";
pub(super) const CHECKPOINT_RUN_ID: &str = "jrun-checkpoint-owner";
pub(super) const FIRST_RESUME_RUN_ID: &str = "jrun-first-resume";
pub(super) const SECOND_RESUME_RUN_ID: &str = "jrun-second-resume";
pub(super) const THIRD_RESUME_RUN_ID: &str = "jrun-third-resume";

pub(super) struct ResumeFailureHost {
    pub(super) inner: PrOpenTestHost,
    run_states: Mutex<HashMap<String, PipelineState>>,
    job_run_states: Mutex<HashMap<String, JobRunState>>,
}

impl ResumeFailureHost {
    pub(super) fn new(inner: PrOpenTestHost) -> Self {
        Self {
            inner,
            run_states: Mutex::new(HashMap::new()),
            job_run_states: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn write_state(&self, state: PipelineState) {
        self.run_states
            .lock()
            .expect("run states lock")
            .insert(state.run_id.clone(), state);
    }

    pub(super) fn set_job_run_state(&self, run_id: &str, state: JobRunState) {
        self.job_run_states
            .lock()
            .expect("job run states lock")
            .insert(run_id.to_string(), state);
    }
}

impl RuntimeHost for ResumeFailureHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        match action {
            "test_partial_repair_failure" => {
                let workspace_path = input
                    .get("workspace_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DispatchError::DeterministicActionFailed {
                        action: action.to_string(),
                        message: "fixture input is missing workspace_path".to_string(),
                    })?;
                let source_dir = Path::new(workspace_path).join("src");
                fs::create_dir_all(&source_dir).map_err(|error| {
                    DispatchError::DeterministicActionFailed {
                        action: action.to_string(),
                        message: format!("create fixture source directory: {error}"),
                    }
                })?;
                fs::write(
                    source_dir.join("repaired.rs"),
                    "pub fn first_repair() {}\npub fn second_repair() {}\n",
                )
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("write fixture partial repair: {error}"),
                })?;
                git(Path::new(workspace_path), &["add", "--", "src/repaired.rs"]);
                Err(DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: "repaired two ownership calls; required macOS validation remains unavailable"
                        .to_string(),
                })
            }
            "pr_failure_handoff" => pr_failure_handoff(self, input).map_err(|error| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                }
            }),
            "test_finish_repair" => {
                let workspace_path = input
                    .get("workspace_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DispatchError::DeterministicActionFailed {
                        action: action.to_string(),
                        message: "fixture input is missing workspace_path".to_string(),
                    })?;
                fs::write(
                    Path::new(workspace_path).join("src/repaired.rs"),
                    "pub fn first_repair() {}\npub fn second_repair() {}\npub fn resumed_repair() {}\n",
                )
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("write resumed fixture repair: {error}"),
                })?;
                self.apply_task_automation_update(
                    TASK_ID,
                    TaskAutomationUpdate {
                        execution_summary: Some(
                            "Outcome: success\nChanges:\n- Finished the resumed repair."
                                .to_string(),
                        ),
                        ..TaskAutomationUpdate::default()
                    },
                )
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                })?;
                Ok(json!({"implemented": true}))
            }
            "git_commit" => {
                crate::executor::automation::vcs::git_commit(self, input).map_err(|error| {
                    DispatchError::DeterministicActionFailed {
                        action: action.to_string(),
                        message: error.to_string(),
                    }
                })
            }
            "test_resolve_rebase_conflict" => {
                let workspace_path = input
                    .get("workspace_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DispatchError::DeterministicActionFailed {
                        action: action.to_string(),
                        message: "conflict recovery is missing workspace_path".to_string(),
                    })?;
                let workspace_path = Path::new(workspace_path);
                fs::write(
                    workspace_path.join("src/lib.rs"),
                    "pub fn base_advanced() {}\npub fn changed() {}\n",
                )
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("write conflict resolution: {error}"),
                })?;
                git(workspace_path, &["add", "src/lib.rs"]);
                git(
                    workspace_path,
                    &["-c", "core.editor=true", "rebase", "--continue"],
                );
                let run_id = input["run_id"].as_str().unwrap();
                let prepared = &input["failed_step_input"];
                self.checkpoint_rebase_recovery(
                    run_id,
                    "sync_base",
                    &json!({
                        "run_id": run_id,
                        "step_id": "sync_base",
                        "workspace_path": workspace_path,
                        "task_ids": [TASK_ID],
                        "head": prepared["head"],
                        "head_sha_before": prepared["head_sha"],
                        "original_base_sha": input["original_base_sha"],
                        "base_ref": prepared["base_ref"],
                        "base_sha": prepared["base_sha"],
                        "remote_sha_before": prepared["remote_sha"],
                        "head_sha": git(workspace_path, &["rev-parse", "HEAD"]),
                        "rewritten": true,
                    }),
                )?;
                Ok(json!({"recovered": true}))
            }
            "git_push" => crate::executor::automation::vcs::push_batch_changes(self, input)
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                }),
            "pr_open" => crate::executor::automation::vcs::pr_open(self, input).map_err(|error| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                }
            }),
            "pr_promote" => {
                crate::executor::automation::vcs::pr_promote(self, input).map_err(|error| {
                    DispatchError::DeterministicActionFailed {
                        action: action.to_string(),
                        message: error.to_string(),
                    }
                })
            }
            _ => Err(DispatchError::DeterministicActionNotRegistered(
                action.to_string(),
            )),
        }
    }

    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        let mut run = self.inner.get_job_run(run_id)?;
        if let (Some(run), Some(state)) = (
            run.as_mut(),
            self.job_run_states
                .lock()
                .expect("job run states lock")
                .get(run_id),
        ) {
            run.state = *state;
        }
        Ok(run)
    }

    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        Ok(self
            .run_states
            .lock()
            .expect("run states lock")
            .get(run_id)
            .cloned())
    }

    fn checkpoint_step(
        &self,
        run_id: &str,
        step_index: u32,
        _step_id: &str,
        output: &Value,
        pipeline_snapshot: &Value,
    ) -> Result<(), DispatchError> {
        let mut states = self.run_states.lock().expect("run states lock");
        let state = states
            .get_mut(run_id)
            .ok_or_else(|| DispatchError::JobExecution(format!("missing state for {run_id}")))?;
        state.record_step(step_index, JobRunState::Success, Some(output.clone()), None);
        state.sync_pipeline(pipeline_snapshot.clone());
        Ok(())
    }

    fn checkpoint_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        self.run_states
            .lock()
            .unwrap()
            .get_mut(run_id)
            .unwrap()
            .rebase_recovery_checkpoints
            .insert(step_id.to_string(), output.clone());
        self.inner.certify_recovery(run_id, step_id, output);
        Ok(())
    }

    fn verify_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        self.inner
            .verify_rebase_recovery(run_id, step_id, checkpoint)
    }

    fn checkpoint_failure_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        failed_step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        let mut states = self.run_states.lock().expect("run states lock");
        let state = states
            .get_mut(run_id)
            .ok_or_else(|| DispatchError::JobExecution(format!("missing state for {run_id}")))?;
        state.record_failure_activity(
            activity_name.to_string(),
            failed_step_id.to_string(),
            output.clone(),
        );
        Ok(())
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.inner.get_task(task_id)
    }

    fn get_task_artifacts(
        &self,
        task_id: &str,
    ) -> Result<Vec<orbit_types::task::TaskArtifact>, OrbitError> {
        self.inner.get_task_artifacts(task_id)
    }

    fn get_task_comments(
        &self,
        task_id: &str,
    ) -> Result<Vec<orbit_types::task::TaskComment>, OrbitError> {
        self.inner.get_task_comments(task_id)
    }

    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<orbit_types::task::TaskPriority>,
        parent_id: Option<&str>,
        batch_id: Option<&str>,
        external_ref: Option<&orbit_types::task::ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        self.inner.list_tasks_filtered(
            status,
            priority,
            parent_id,
            batch_id,
            external_ref,
            has_external_ref_system,
        )
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        self.inner.apply_task_automation_update(task_id, update)
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        self.inner.update_task_from_activity(task_id, update)
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        self.inner.run_private_vcs_operation(operation, input)
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        Some("system-test".to_string())
    }
}

pub(super) fn task_owned_by(run_id: &str) -> orbit_types::task::Task {
    let mut task = batch_task(
        TASK_ID,
        "Preserve partial repair",
        "Outcome: failed\nChanges:\n- Repaired two calls before required validation failed.",
    );
    task.job_run_id = Some(run_id.to_string());
    task
}

fn resumed_failure_input(workspace_path: &std::path::Path, run_id: &str) -> Value {
    json!({
        "failed_step_id": "implement_bundle",
        "activity_name": "agent_implement",
        "error_code": "macos_validation_unavailable",
        "error_message": "repaired two ownership calls; required macOS validation remains unavailable",
        "run_id": run_id,
        "job_input": {
            "task_ids": [TASK_ID],
            "base_branch": "agent-main",
            "base_sync": "local",
        },
        "pipeline": {
            "worktree": {
                "workspace_path": workspace_path,
                "job_run_id": CHECKPOINT_RUN_ID,
                "base_ref": "agent-main",
            },
        },
    })
}

pub(super) fn deterministic_activity(action: &str) -> ActivityV2 {
    ActivityV2 {
        description: format!("test action {action}"),
        input_schema_json: Value::Null,
        output_schema_json: Value::Null,
        fs_profile: None,
        spec: ActivityV2Spec::Deterministic(DeterministicSpec {
            action: action.to_string(),
            config: Value::Null,
        }),
    }
}

pub(super) fn deterministic_step(
    id: &str,
    action: &str,
    default_input: Option<Value>,
) -> JobV2Step {
    JobV2Step {
        id: id.to_string(),
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Target(TargetStep {
            spec: deterministic_activity(action).spec,
            activity_name: None,
            input_schema_json: None,
            fs_profile: None,
            default_input,
            timeout_seconds: 0,
            session: None,
        }),
    }
}

fn resumed_pr_job() -> JobV2 {
    JobV2 {
        state: JobScheduleState::Enabled,
        default_input: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        failure_activity: Some("pr_failure_handoff".to_string()),
        resolved_failure_activity: Some(deterministic_activity("pr_failure_handoff")),
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps: vec![
            deterministic_step("worktree", "unused_worktree_setup", None),
            deterministic_step(
                "implement_bundle",
                "test_partial_repair_failure",
                Some(json!({
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                })),
            ),
        ],
    }
}

pub(super) fn resumed_pr_delivery_job() -> JobV2 {
    JobV2 {
        state: JobScheduleState::Enabled,
        default_input: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        failure_activity: None,
        resolved_failure_activity: None,
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps: vec![
            deterministic_step("worktree", "unused_worktree_setup", None),
            deterministic_step(
                "implement_bundle",
                "test_finish_repair",
                Some(json!({
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                })),
            ),
            deterministic_step(
                "commit",
                "git_commit",
                Some(json!({
                    "scope": "all",
                    "job_run_id": "{{ steps.worktree.output.job_run_id }}",
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                    "base_sha": "{{ steps.worktree.output.base_sha }}",
                })),
            ),
            deterministic_step(
                "push",
                "git_push",
                Some(json!({
                    "job_run_id": "{{ steps.worktree.output.job_run_id }}",
                    "completed_task_ids": [TASK_ID],
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                    "branch": "orbit/test-batch",
                })),
            ),
            deterministic_step(
                "pr_open",
                "pr_open",
                Some(json!({
                    "job_run_id": "{{ steps.worktree.output.job_run_id }}",
                    "completed_task_ids": [TASK_ID],
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                    "head": "orbit/test-batch",
                    "base": "agent-main",
                    "base_ref": "agent-main",
                    "base_sha": "{{ steps.worktree.output.base_sha }}",
                })),
            ),
            deterministic_step(
                "promote_tasks",
                "pr_promote",
                Some(json!({
                    "job_run_id": "{{ steps.worktree.output.job_run_id }}",
                    "completed_task_ids": [TASK_ID],
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                    "head": "orbit/test-batch",
                    "base": "agent-main",
                    "base_ref": "agent-main",
                    "base_sha": "{{ steps.worktree.output.base_sha }}",
                    "pr_number": "{{ steps.pr_open.output.pr_number }}",
                    "pr_url": "{{ steps.pr_open.output.pr_url }}",
                })),
            ),
        ],
    }
}

pub(super) fn completed_worktree_checkpoint(
    run_id: &str,
    workspace_path: &std::path::Path,
) -> PipelineState {
    let base_sha = git(workspace_path, &["rev-parse", "HEAD"]);
    let input = json!({
        "task_ids": [TASK_ID],
        "base_branch": "agent-main",
        "base_sync": "local",
    });
    let worktree = json!({
        "workspace_path": workspace_path,
        "job_run_id": CHECKPOINT_RUN_ID,
        "base_ref": "agent-main",
        "base_sha": base_sha,
    });
    let mut state = PipelineState::new(run_id.to_string(), "task_pr_pipeline".to_string(), input);
    state.record_step(0, JobRunState::Success, Some(worktree.clone()), None);
    state.sync_pipeline(json!({ "worktree": worktree }));
    state
}

pub(super) fn record_preservation_evidence(
    state: &mut PipelineState,
    handoff_run_id: &str,
    head_sha: &str,
) {
    let base_sha = state.step_outputs[&0]["base_sha"]
        .as_str()
        .expect("worktree base sha");
    state.record_failure_activity(
        "pr_failure_handoff".to_string(),
        "implement_bundle".to_string(),
        json!({
            "phase": "failure_handoff",
            "decision": "blocked_failure_pr",
            "task_id": TASK_ID,
            "handoff_run_id": handoff_run_id,
            "checkpoint_owner": CHECKPOINT_RUN_ID,
            "preservation_commit_created": true,
            "head_sha": head_sha,
            "original_base_sha": base_sha,
        }),
    );
}

#[test]
fn repeated_resume_failure_publishes_partial_repair_to_the_existing_pr() {
    let workspace = no_diff_pr_workspace();
    let source_state = completed_worktree_checkpoint(SECOND_RESUME_RUN_ID, &workspace.repo);
    let host = ResumeFailureHost::new(
        PrOpenTestHost::new(
            vec![task_owned_by(CHECKPOINT_RUN_ID)],
            workspace.repo.clone(),
        )
        .with_existing_pr()
        .with_job_run(CHECKPOINT_RUN_ID, None)
        .with_job_run(FIRST_RESUME_RUN_ID, Some(CHECKPOINT_RUN_ID))
        .with_job_run(SECOND_RESUME_RUN_ID, Some(FIRST_RESUME_RUN_ID))
        .with_job_run(THIRD_RESUME_RUN_ID, Some(SECOND_RESUME_RUN_ID)),
    );
    host.write_state(source_state.clone());
    let sink: Arc<dyn AuditSink> = Arc::new(NullSink);
    let writer = Arc::new(V2AuditWriter::new(SECOND_RESUME_RUN_ID, "test-agent", sink));

    let error = execute_job_with_resume(
        &resumed_pr_job(),
        json!({
            "task_ids": [TASK_ID],
            "base_branch": "agent-main",
            "base_sync": "local",
        }),
        SECOND_RESUME_RUN_ID,
        writer,
        &host,
        Some(&source_state),
    )
    .expect_err("the resumed implementation failure remains authoritative");

    assert!(
        error
            .to_string()
            .contains("required macOS validation remains unavailable"),
        "the original implementation blocker stays authoritative: {error}",
    );
    assert!(
        workspace.repo.join("src/repaired.rs").exists(),
        "the resumed implementation produced its partial repair",
    );
    assert_eq!(host.inner.task_status(TASK_ID), TaskStatus::Blocked);
    let recovered_head = git(&workspace.repo, &["rev-parse", "HEAD"]);
    assert_ne!(
        recovered_head,
        git(&workspace.repo, &["rev-parse", "agent-main"]),
        "failure handoff commits the partial repair",
    );
    let push = host
        .inner
        .vcs_calls()
        .into_iter()
        .find(|call| call.operation == PUSH_OPERATION)
        .expect("the recovered candidate is published");
    assert_eq!(push.input["branch"], json!("orbit/test-batch"));
    assert_eq!(push.input["repo_root"], json!(workspace.repo));
    assert!(
        !host
            .inner
            .vcs_calls()
            .iter()
            .any(|call| call.operation == PR_CREATE_OPERATION),
        "the existing PR is reused rather than replaced",
    );
    let blocker = host
        .inner
        .comments_for(TASK_ID)
        .pop()
        .expect("precise failure blocker");
    assert!(blocker.message.contains(SECOND_RESUME_RUN_ID));
    assert!(blocker.message.contains("implement_bundle"));
    assert!(
        blocker.message.contains("pipeline_step_failed"),
        "{}",
        blocker.message
    );
    assert!(
        blocker
            .message
            .contains("required macOS validation remains unavailable")
    );

    let preserved_head = recovered_head;
    let preserved_state = host
        .read_run_state(SECOND_RESUME_RUN_ID)
        .expect("read preserved state")
        .expect("preserved state exists");
    assert_eq!(
        preserved_state
            .failure_activity_checkpoint
            .as_ref()
            .expect("failure handoff checkpoint")
            .output["head_sha"],
        preserved_head,
    );

    let mut resumed_state = preserved_state.clone();
    resumed_state.run_id = THIRD_RESUME_RUN_ID.to_string();
    host.write_state(resumed_state.clone());
    host.apply_task_automation_update(
        TASK_ID,
        TaskAutomationUpdate {
            status: Some(TaskStatus::InProgress),
            ..TaskAutomationUpdate::default()
        },
    )
    .expect("re-admit task for resume");

    let writer = Arc::new(V2AuditWriter::new(
        THIRD_RESUME_RUN_ID,
        "test-agent",
        Arc::new(NullSink),
    ));
    let delivered = execute_job_with_resume(
        &resumed_pr_delivery_job(),
        json!({
            "task_ids": [TASK_ID],
            "base_branch": "agent-main",
            "base_sync": "local",
        }),
        THIRD_RESUME_RUN_ID,
        writer,
        &host,
        Some(&resumed_state),
    )
    .expect("resume accepts the exact Orbit preservation commit");

    assert!(delivered.success);
    assert_eq!(delivered.pipeline["commit"]["decision"], "performed");
    assert_eq!(delivered.pipeline["pr_open"]["decision"], "reused");
    assert_eq!(delivered.pipeline["pr_open"]["pr_number"], "42");
    assert_eq!(host.inner.task_status(TASK_ID), TaskStatus::Review);
    assert_eq!(
        git(&workspace.repo, &["rev-parse", "HEAD^1"]),
        preserved_head,
        "the resumed commit retains the preservation commit as its parent",
    );
    assert!(
        !host
            .inner
            .vcs_calls()
            .iter()
            .any(|call| call.operation == PR_CREATE_OPERATION),
        "delivery reuses the failure handoff PR",
    );
}

#[test]
fn unrelated_run_cannot_use_another_runs_checkpoint_to_publish() {
    let workspace = no_diff_pr_workspace();
    fs::write(workspace.repo.join("unrelated.txt"), "must stay local\n")
        .expect("write unrelated candidate");
    let host = PrOpenTestHost::new(
        vec![task_owned_by(CHECKPOINT_RUN_ID)],
        workspace.repo.clone(),
    )
    .with_job_run(CHECKPOINT_RUN_ID, None)
    .with_job_run("jrun-unrelated", None);

    let error = pr_failure_handoff(
        &host,
        &resumed_failure_input(&workspace.repo, "jrun-unrelated"),
    )
    .expect_err("an unrelated run cannot inherit checkpoint ownership");

    assert!(
        error.to_string().contains("not a retry descendant"),
        "{error}"
    );
    assert!(host.vcs_calls().is_empty());
    assert_eq!(host.task_status(TASK_ID), TaskStatus::InProgress);
    assert!(
        git(&workspace.repo, &["status", "--porcelain"]).contains("unrelated.txt"),
        "ownership refusal occurs before candidate mutation",
    );
}

#[test]
fn superseded_resume_cannot_publish_after_task_ownership_moves() {
    let workspace = no_diff_pr_workspace();
    fs::write(workspace.repo.join("superseded.txt"), "must stay local\n")
        .expect("write superseded candidate");
    let host = PrOpenTestHost::new(
        vec![task_owned_by("jrun-superseding-owner")],
        workspace.repo.clone(),
    )
    .with_job_run(CHECKPOINT_RUN_ID, None)
    .with_job_run(FIRST_RESUME_RUN_ID, Some(CHECKPOINT_RUN_ID));

    let error = pr_failure_handoff(
        &host,
        &resumed_failure_input(&workspace.repo, FIRST_RESUME_RUN_ID),
    )
    .expect_err("a superseded resume cannot publish the new owner's task");

    let message = error.to_string();
    assert!(message.contains("jrun-superseding-owner"), "{message}");
    assert!(message.contains(FIRST_RESUME_RUN_ID), "{message}");
    assert!(message.contains(CHECKPOINT_RUN_ID), "{message}");
    assert!(host.vcs_calls().is_empty());
    assert_eq!(host.task_status(TASK_ID), TaskStatus::InProgress);
    assert!(
        git(&workspace.repo, &["status", "--porcelain"]).contains("superseded.txt"),
        "ownership refusal occurs before candidate mutation",
    );
}

#[test]
fn original_run_ownership_still_authorizes_failure_handoff() {
    let workspace = no_diff_pr_workspace();
    fs::write(workspace.repo.join("original.txt"), "original candidate\n")
        .expect("write original candidate");
    git(&workspace.repo, &["add", "--", "original.txt"]);
    let host = PrOpenTestHost::new(
        vec![task_owned_by(CHECKPOINT_RUN_ID)],
        workspace.repo.clone(),
    );

    let recovered = pr_failure_handoff(
        &host,
        &resumed_failure_input(&workspace.repo, CHECKPOINT_RUN_ID),
    )
    .expect("the original run retains its direct ownership authority");

    assert_eq!(recovered["decision"], json!("blocked_failure_pr"));
    assert_eq!(recovered["committed_files"], json!(["original.txt"]));
    assert_eq!(host.task_status(TASK_ID), TaskStatus::Blocked);
}

#[test]
fn resumed_delivery_tail_preserves_the_published_pr() {
    let workspace = pr_workspace();
    let mut task = review_batch_task(TASK_ID, Some("codex"), None);
    task.job_run_id = Some(CHECKPOINT_RUN_ID.to_string());
    let host = PrOpenTestHost::new(vec![task], workspace.repo.clone())
        .with_existing_pr()
        .with_job_run(CHECKPOINT_RUN_ID, None)
        .with_job_run(FIRST_RESUME_RUN_ID, Some(CHECKPOINT_RUN_ID));
    let head_before = git(&workspace.repo, &["rev-parse", "HEAD"]);
    let mut input = resumed_failure_input(&workspace.repo, FIRST_RESUME_RUN_ID);
    input["failed_step_id"] = json!("complete_pr");
    input["activity_name"] = json!("pr_complete");
    input["error_code"] = json!("merge_gate_failed");
    input["error_message"] = json!("required status check remains pending");

    let recovered = pr_failure_handoff(&host, &input)
        .expect("retry lineage authorizes completion-tail preservation");

    assert_eq!(recovered["decision"], json!("review_completion_failure"));
    assert_eq!(recovered["pr_number"], json!("42"));
    assert_eq!(host.task_status(TASK_ID), TaskStatus::Review);
    assert_eq!(git(&workspace.repo, &["rev-parse", "HEAD"]), head_before);
    assert!(host.vcs_calls().is_empty());
}
