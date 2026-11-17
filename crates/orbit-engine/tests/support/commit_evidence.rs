//! Shared in-memory host and report fixtures for commit-verifier scenarios.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{Task, TaskArtifact, TaskComment, TaskPriority, TaskStatus};
use serde_json::{Value, json};

use super::{RUN_ID, TASK_ID};

pub(super) struct VerifierHost {
    repo: PathBuf,
    pub(super) tasks: Mutex<BTreeMap<String, Task>>,
    comments: Mutex<BTreeMap<String, Vec<TaskComment>>>,
    pub(super) artifacts: Mutex<BTreeMap<String, Vec<TaskArtifact>>>,
    pub(super) worker: Option<orbit_types::tool::WorkerInvocation>,
    pub(super) state: Mutex<Option<orbit_types::workflow::PipelineState>>,
    pub(super) unknown_artifact_read: Mutex<Option<(String, usize)>>,
    pub(super) bulk_reads: Mutex<usize>,
    pr_config: Mutex<orbit_engine::PrConfig>,
    pub(super) rejected_findings: Mutex<Vec<orbit_types::workflow::handoff::FindingInvalidReport>>,
    // `workflow.required_validation_commands`. Empty unless a case sets it.
    required_commands: Mutex<Vec<String>>,
}

impl VerifierHost {
    pub(super) fn new(repo: &Path, task: Task) -> Self {
        Self {
            repo: repo.to_path_buf(),
            tasks: Mutex::new(BTreeMap::from([(task.id.clone(), task)])),
            comments: Mutex::default(),
            artifacts: Mutex::default(),
            worker: None,
            state: Mutex::default(),
            unknown_artifact_read: Mutex::default(),
            bulk_reads: Mutex::default(),
            pr_config: Mutex::default(),
            rejected_findings: Mutex::default(),
            required_commands: Mutex::default(),
        }
    }

    pub(super) fn set_required_commands(&self, commands: &[&str]) {
        *self.required_commands.lock().unwrap() = commands
            .iter()
            .map(|command| (*command).to_string())
            .collect();
    }

    pub(super) fn set_conventions(&self, conventions: orbit_types::workflow::DeliveryConventions) {
        self.pr_config.lock().unwrap().conventions = conventions;
    }

    pub(super) fn set_comments(&self, task_id: &str, comments: Vec<TaskComment>) {
        self.comments
            .lock()
            .unwrap()
            .insert(task_id.to_string(), comments);
    }

    pub(super) fn set_artifacts(&self, task_id: &str, artifacts: Vec<TaskArtifact>) {
        self.artifacts
            .lock()
            .unwrap()
            .insert(task_id.to_string(), artifacts);
    }
}

impl orbit_engine::test_support::NullRuntimeHost for VerifierHost {
    fn pr_config(&self) -> orbit_engine::PrConfig {
        self.pr_config.lock().unwrap().clone()
    }

    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        self.worker.clone()
    }

    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Result<(), OrbitError> {
        let mut artifacts = self.artifacts.lock().unwrap();
        let logs = artifacts.entry(TASK_ID.to_string()).or_default();
        logs.retain(|artifact| artifact.path != path);
        logs.push(TaskArtifact::from_text(
            path,
            String::from_utf8(content).expect("fixture JSON is UTF-8"),
        ));
        Ok(())
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        Ok(self
            .comments
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
            .unwrap_or_default())
    }

    fn get_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        if let Some((fault_path, remaining)) = &mut *self.unknown_artifact_read.lock().unwrap()
            && fault_path == path
        {
            *remaining -= 1;
            if *remaining == 0 {
                return Err(OrbitError::OutcomeUnknown {
                    mcp_call_id: "lost-artifact-read".into(),
                    message: "named artifact reply lost".into(),
                });
            }
        }
        let artifact = self
            .artifacts
            .lock()
            .unwrap()
            .get(task_id)
            .and_then(|artifacts| artifacts.iter().find(|artifact| artifact.path == path))
            .cloned();
        if let Some(artifact) = &artifact {
            artifact.validate_inline_size(task_id)?;
        }
        Ok(artifact)
    }

    fn get_task_artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        *self.bulk_reads.lock().unwrap() += 1;
        Ok(self
            .artifacts
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
            .unwrap_or_default())
    }

    fn read_run_state(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::PipelineState>, OrbitError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .as_ref()
            .filter(|state| state.run_id == run_id)
            .cloned())
    }

    fn get_job_run(
        &self,
        _run_id: &str,
    ) -> Result<Option<orbit_types::workflow::JobRun>, OrbitError> {
        Ok(None)
    }

    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        batch_id: Option<&str>,
        _external_ref: Option<&orbit_types::task::ExternalRef>,
        _has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        Ok(self
            .tasks
            .lock()
            .unwrap()
            .values()
            .filter(|task| status.is_none_or(|status| task.status == status))
            .filter(|task| priority.is_none_or(|priority| task.priority == priority))
            .filter(|task| parent_id.is_none_or(|parent_id| task.parent_id() == Some(parent_id)))
            .filter(|task| {
                batch_id.is_none_or(|batch_id| task.job_run_id.as_deref() == Some(batch_id))
            })
            .cloned()
            .collect())
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: orbit_engine::TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        if let Some(status) = update.status {
            task.status = status;
        }
        if let Some(execution_summary) = update.execution_summary {
            task.execution_summary = execution_summary;
        }
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn reject_invalid_finding(
        &self,
        report: &orbit_types::workflow::handoff::FindingInvalidReport,
        _base_branch: &str,
    ) -> Result<(), OrbitError> {
        self.rejected_findings.lock().unwrap().push(report.clone());
        Ok(())
    }

    fn required_validation_commands(&self) -> Vec<String> {
        self.required_commands.lock().unwrap().clone()
    }
}

pub(super) fn verifier_validation_log(
    head: &str,
    command: &str,
    exit_code: i64,
    output: &str,
) -> Value {
    json!({
        "run_id": RUN_ID,
        "tested_head": head,
        "command": command,
        "exit_code": exit_code,
        "output": output,
    })
}

pub(super) fn verifier_no_diff_report(head: &str, command: &str, reason: &str) -> Value {
    json!({
        "schema_version": 1,
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "tested_head": head,
        "reason": reason,
        "validation": [{
            "command": command,
            "exit_code": 0,
            "log_artifact": "validation.json"
        }]
    })
}

pub(super) fn verifier_already_landed_report(head: &str) -> Value {
    json!({
        "schema_version": 1,
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "tested_head": head,
        "covering_commit": head,
        "covering_task_id": TASK_ID,
        "scope": {
            "title": "Test Task",
            "description": "",
            "acceptance_criteria": ["Criterion 1"],
            "plan": "",
            "context_files": ["file:README.md"],
            "tags": [],
            "relations": [],
            "required_tools": [],
            "type": "bug",
            "comments": []
        },
        "required_commands": ["make test"],
        "validation": [{
            "command": "make test",
            "outcome": "passed",
            "role": "required",
            "log_artifact": "validation.json"
        }],
        "criteria_evidence": ["Verified criterion"]
    })
}
