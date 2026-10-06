//! Task commit boundary contracts: staged rename delivery and refusal of
//! unknown fields for no-diff and already-landed evidence [ORB-13881].

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

fn git_output(dir: &Path, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git execution");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn init_git_repo(dir: &Path) {
    let git = |args: &[&str]| git_output(dir, args);
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
    git_output(dir, &["rev-parse", "HEAD"])
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
fn per_task_commit_delivers_both_sides_of_a_staged_rename() {
    let temp = tempdir().expect("create tempdir");
    let repo = temp.path();
    init_git_repo(repo);
    git_output(repo, &["config", "diff.renames", "true"]);
    git_output(repo, &["mv", "README.md", "renamed.md"]);
    let mut task = fixture_task();
    task.context_files.push("file:renamed.md".to_string());
    let host = VerifierHost::new(repo, task);

    let result = action(
        &host,
        &json!({
            "scope": "per_task",
            "job_run_id": RUN_ID,
            "workspace_path": repo,
            "completed_task_ids": [TASK_ID],
        }),
    )
    .expect("deliver staged rename");

    assert_eq!(result["committed_task_ids"], json!([TASK_ID]));
    assert_eq!(
        git_output(
            repo,
            &[
                "diff",
                "--no-renames",
                "--name-status",
                "HEAD~1",
                "HEAD",
                "--"
            ],
        ),
        "D\tREADME.md\nA\trenamed.md",
        "ORB-14097: the task commit must include the rename source deletion and destination addition"
    );
    assert_eq!(git_output(repo, &["show", "HEAD:renamed.md"]), "base");
    assert!(
        git_output(repo, &["status", "--porcelain", "--untracked-files=all"]).is_empty(),
        "ORB-14097: no staged deletion may leak into a later task's commit"
    );
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

/// Already-landed evidence that is valid except for the overrides applied.
fn already_landed_refusal(
    host: &VerifierHost,
    repo: &Path,
    head: &str,
    mutate: impl FnOnce(&mut Value),
) -> String {
    let mut report = json!({
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
    mutate(&mut report);
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
            artifact("already-landed.json", report),
            artifact("validation.json", log),
        ],
    );
    action(host, &commit_input(repo, head))
        .expect_err("malformed already-landed evidence must be refused")
        .to_string()
}

#[test]
fn commit_verifier_refusal_names_already_landed_field_contract() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());

    let message = already_landed_refusal(&host, temp.path(), &head, |report| {
        report["criteria_evidence"] = json!([{"criterion": "Criterion 1", "evidence": "ok"}]);
    });
    assert!(
        message.contains("invalid already-landed.json")
            && message.contains("criteria_evidence must be an array of non-empty strings"),
        "object-shaped criteria_evidence must be refused with the string-per-criterion contract, got: {message}"
    );

    let message = already_landed_refusal(&host, temp.path(), &head, |report| {
        report["validation"][0]["role"] = json!("acceptance");
    });
    assert!(
        message.contains("invalid already-landed.json")
            && message.contains("required, expected_failure, excluded, superseded"),
        "unknown validation role must be refused with the accepted roles, got: {message}"
    );
}

#[test]
fn commit_verifier_keeps_refusing_unaccepted_already_landed_evidence() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());

    type Mutation = fn(&mut Value);
    let cases: [(&str, Mutation); 4] = [
        ("criteria_evidence count differs from the criteria", |r| {
            r["criteria_evidence"] = json!(["a", "b"]);
        }),
        ("empty criteria_evidence string", |r| {
            r["criteria_evidence"] = json!(["  "]);
        }),
        ("non-required role for a required command", |r| {
            r["validation"][0]["role"] = json!("superseded");
        }),
        ("non-passed outcome", |r| {
            r["validation"][0]["outcome"] = json!("failed");
        }),
    ];
    for (case, mutate) in cases {
        let message = already_landed_refusal(&host, temp.path(), &head, mutate);
        assert!(
            message.contains("already_landed_unverified"),
            "{case} must stay refused, got: {message}"
        );
    }
}

/// A `no-diff-expected` task skips a clean stage and commits an unexpected
/// diff. The commit is the normal shipment commit; `sync_base` remains the
/// conflict boundary for that diff [ORB-14247].
#[test]
fn no_diff_expected_commits_an_unexpected_diff_and_skips_a_clean_tree() {
    let clean = tempdir().expect("create tempdir");
    init_git_repo(clean.path());
    let clean_head = git_head(clean.path());
    let mut clean_task = fixture_task();
    clean_task.tags = vec!["no-diff-expected".to_string()];
    let clean_host = VerifierHost::new(clean.path(), clean_task);
    let skipped = action(&clean_host, &commit_input(clean.path(), &clean_head))
        .expect("a clean no-diff-expected tree skips the commit");
    assert_eq!(skipped["decision"], "skipped_no_diff_expected");
    assert_eq!(skipped["committed"], false);
    assert_eq!(skipped["skipped_no_diff_expected"], true);
    assert_eq!(git_head(clean.path()), clean_head);

    let dirty = tempdir().expect("create tempdir");
    init_git_repo(dirty.path());
    let dirty_head = git_head(dirty.path());
    let mut dirty_task = fixture_task();
    dirty_task.tags = vec!["no-diff-expected".to_string()];
    dirty_task.execution_summary =
        "Outcome: success\n\nThe review edited the tree, so delivery commits that diff."
            .to_string();
    let dirty_host = VerifierHost::new(dirty.path(), dirty_task);
    fs::write(dirty.path().join("README.md"), "review edit\n").expect("dirty tree");
    let committed = action(&dirty_host, &commit_input(dirty.path(), &dirty_head))
        .expect("an unexpected diff on a no-diff-expected task commits");
    assert_eq!(committed["decision"], "performed", "{committed}");
    assert_eq!(committed["committed"], true, "{committed}");
    assert_eq!(committed["skipped_no_diff_expected"], false, "{committed}");
    assert_ne!(git_head(dirty.path()), dirty_head);
    assert_eq!(
        fs::read_to_string(dirty.path().join("README.md")).unwrap(),
        "review edit\n"
    );
}
