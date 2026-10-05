//! Commit verifier boundary contracts: refusal of unknown fields for no-diff
//! and already-landed evidence, preserving deny_unknown_fields [ORB-13881].

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::{RuntimeHost, execute_deterministic_action};
use orbit_types::task::{Task, TaskArtifact, TaskComment, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};
use tempfile::tempdir;

const TASK_ID: &str = "T1";
const RUN_ID: &str = "jrun-verifier-test";

struct VerifierHost {
    repo: PathBuf,
    tasks: Mutex<BTreeMap<String, Task>>,
    comments: Mutex<BTreeMap<String, Vec<TaskComment>>>,
    artifacts: Mutex<BTreeMap<String, Vec<TaskArtifact>>>,
}

impl VerifierHost {
    fn new(repo: &Path, task: Task) -> Self {
        Self {
            repo: repo.to_path_buf(),
            tasks: Mutex::new(BTreeMap::from([(task.id.clone(), task)])),
            comments: Mutex::default(),
            artifacts: Mutex::default(),
        }
    }

    fn set_artifacts(&self, task_id: &str, artifacts: Vec<TaskArtifact>) {
        self.artifacts
            .lock()
            .unwrap()
            .insert(task_id.to_string(), artifacts);
    }
}

impl RuntimeHost for VerifierHost {
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

    fn get_task_artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(self
            .artifacts
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
            .unwrap_or_default())
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
}

fn fixture_task() -> Task {
    let now = Utc::now();
    Task {
        job_run_machine: None,
        id: TASK_ID.to_string(),
        title: "Test Task".to_string(),
        description: String::new(),
        acceptance_criteria: vec!["Criterion 1".to_string()],
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: "Outcome: success\n\nNo change needed.".to_string(),
        context_files: vec!["file:README.md".to_string()],
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::InProgress,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Bug,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: Some(RUN_ID.to_string()),
        crew: None,
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}

fn init_git_repo(dir: &Path) {
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git execution");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    git(&["config", "user.name", "Test User"]);
    git(&["config", "user.email", "test@example.invalid"]);
    let hooks = dir.join(".git").join("orbit-test-empty-hooks");
    fs::create_dir_all(&hooks).expect("create empty hooks dir");
    git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
    fs::write(dir.join("README.md"), "base\n").expect("write README");
    git(&["add", "README.md"]);
    git(&["commit", "-m", "initial commit [T1]"]);
}

fn git_head(dir: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse HEAD");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn action(host: &VerifierHost, input: &Value) -> Result<Value, OrbitError> {
    execute_deterministic_action(
        host,
        "git_commit",
        &json!({}),
        input,
        false,
        &HashMap::new(),
        None,
    )
}

fn commit_input(repo: &Path, head: &str) -> Value {
    json!({
        "scope": "all",
        "job_run_id": RUN_ID,
        "workspace_path": repo.to_str().unwrap(),
        "base_sha": head,
        "verify_already_landed": true,
    })
}

fn artifact(path: &str, content: Value) -> TaskArtifact {
    TaskArtifact::from_text(path, serde_json::to_string_pretty(&content).unwrap())
}

#[test]
fn commit_verifier_refuses_no_diff_with_covering_commit_or_unknown_fields() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());

    // 1. Evidence carrying `covering_commit` (the exact failure from ORB-13778 / ORB-13848)
    let no_diff_with_covering_commit = json!({
        "schema_version": 1,
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "tested_head": head,
        "reason": "Genuine no-op.",
        "covering_commit": head,
        "validation": [{
            "command": "make test",
            "exit_code": 0,
            "log_artifact": "validation.json"
        }]
    });
    let log = json!({
        "run_id": RUN_ID,
        "tested_head": head,
        "command": "make test",
        "exit_code": 0,
        "output": "ok"
    });

    host.set_artifacts(
        TASK_ID,
        vec![
            artifact("no-diff.json", no_diff_with_covering_commit),
            artifact("validation.json", log.clone()),
        ],
    );

    let error = action(&host, &commit_input(temp.path(), &head))
        .expect_err("no-diff.json carrying covering_commit must be refused by the verifier");
    let message = error.to_string();
    assert!(
        message.contains("unknown field `covering_commit`"),
        "expected refusal mentioning unknown field covering_commit, got: {message}"
    );

    // 2. Evidence carrying any other unknown field
    let no_diff_with_unknown_field = json!({
        "schema_version": 1,
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "tested_head": head,
        "reason": "Genuine no-op.",
        "unexpected_field": "disallowed",
        "validation": [{
            "command": "make test",
            "exit_code": 0,
            "log_artifact": "validation.json"
        }]
    });
    host.set_artifacts(
        TASK_ID,
        vec![
            artifact("no-diff.json", no_diff_with_unknown_field),
            artifact("validation.json", log),
        ],
    );

    let error = action(&host, &commit_input(temp.path(), &head))
        .expect_err("no-diff.json carrying unknown field must be refused by the verifier");
    let message = error.to_string();
    assert!(
        message.contains("unknown field `unexpected_field`"),
        "expected refusal mentioning unknown field unexpected_field, got: {message}"
    );
}

#[test]
fn commit_verifier_refuses_already_landed_with_unknown_fields() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());

    let already_landed_with_unknown_field = json!({
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
        "criteria_evidence": ["Verified criterion"],
        "unexpected_field": "disallowed"
    });
    let log = json!({
        "run_id": RUN_ID,
        "tested_head": head,
        "command": "make test",
        "exit_code": 0,
        "output": "ok"
    });

    host.set_artifacts(
        TASK_ID,
        vec![
            artifact("already-landed.json", already_landed_with_unknown_field),
            artifact("validation.json", log),
        ],
    );

    let error = action(&host, &commit_input(temp.path(), &head))
        .expect_err("already-landed.json carrying unknown field must be refused by the verifier");
    let message = error.to_string();
    assert!(
        message.contains("unknown field `unexpected_field`"),
        "expected refusal mentioning unknown field unexpected_field, got: {message}"
    );
}

#[test]
fn commit_verifier_accepts_valid_no_diff_evidence() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());

    let valid_no_diff = json!({
        "schema_version": 1,
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "tested_head": head,
        "reason": "Genuine no-op.",
        "validation": [{
            "command": "make test",
            "exit_code": 0,
            "log_artifact": "validation.json"
        }]
    });
    let log = json!({
        "run_id": RUN_ID,
        "tested_head": head,
        "command": "make test",
        "exit_code": 0,
        "output": "ok"
    });

    host.set_artifacts(
        TASK_ID,
        vec![
            artifact("no-diff.json", valid_no_diff),
            artifact("validation.json", log),
        ],
    );

    let result = action(&host, &commit_input(temp.path(), &head))
        .expect("valid no-diff evidence must be accepted by the verifier");
    assert_eq!(result["decision"], "verified_no_diff");
    assert_eq!(result["skipped_no_diff_expected"], true);
    assert_eq!(result["committed"], false);
}

#[test]
fn commit_verifier_accepts_valid_already_landed_evidence() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());

    let valid_already_landed = json!({
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
    });
    let log = json!({
        "run_id": RUN_ID,
        "tested_head": head,
        "command": "make test",
        "exit_code": 0,
        "output": "ok"
    });

    host.set_artifacts(
        TASK_ID,
        vec![
            artifact("already-landed.json", valid_already_landed),
            artifact("validation.json", log),
        ],
    );

    let result = action(&host, &commit_input(temp.path(), &head))
        .expect("valid already-landed evidence must be accepted by the verifier");
    assert_eq!(result["decision"], "verified_already_landed");
    assert_eq!(result["skipped_no_diff_expected"], true);
    assert_eq!(result["committed"], false);
}
