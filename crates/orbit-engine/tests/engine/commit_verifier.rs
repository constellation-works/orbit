//! Task commit boundary contracts: staged rename delivery and refusal of
//! unknown fields for no-diff and already-landed evidence [ORB-13881], and
//! refusal of nested repositories that would deliver as gitlinks [ORB-15265].

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
        crew_source: None,
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

#[cfg(unix)]
#[test]
fn commit_verifier_accepts_already_landed_through_symlinked_parent_workspace() {
    let temp = tempdir().expect("create tempdir");
    let real_parent = temp.path().join("real");
    fs::create_dir_all(&real_parent).expect("create real parent");
    let repo = real_parent.join("repo");
    fs::create_dir_all(&repo).expect("create repo");
    init_git_repo(&repo);
    let head = git_head(&repo);

    let symlink_parent = temp.path().join("symlink_parent");
    std::os::unix::fs::symlink(&real_parent, &symlink_parent).expect("create symlink");
    let symlinked_repo = symlink_parent.join("repo");

    let host = VerifierHost::new(&symlinked_repo, fixture_task());

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

    let result = action(&host, &commit_input(&symlinked_repo, &head))
        .expect("already-landed evidence through symlinked workspace must be accepted");
    assert_eq!(result["decision"], "verified_already_landed");
    assert_eq!(result["skipped_no_diff_expected"], true);
    assert_eq!(result["committed"], false);
}

#[cfg(unix)]
#[test]
fn commit_verifier_refuses_already_landed_with_escaping_anchor_symlink() {
    let temp = tempdir().expect("create tempdir");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).expect("create repo");
    let outside = temp.path().join("outside.txt");
    fs::write(&outside, "outside\n").expect("write outside");
    let escaping_link = repo.join("escaping.txt");
    std::os::unix::fs::symlink(&outside, &escaping_link).expect("create symlink");

    let git = |args: &[&str]| git_output(&repo, args);
    git(&["init"]);
    git(&["config", "user.name", "Test User"]);
    git(&["config", "user.email", "test@example.invalid"]);
    let hooks = repo.join(".git").join("orbit-test-empty-hooks");
    fs::create_dir_all(&hooks).expect("create empty hooks dir");
    git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
    fs::write(repo.join("README.md"), "base\n").expect("write README");
    git(&["add", "README.md", "escaping.txt"]);
    git(&["commit", "-m", "initial commit [T1]"]);
    let head = git_head(&repo);

    let mut task = fixture_task();
    task.context_files = vec!["file:escaping.txt".to_string()];
    let host = VerifierHost::new(&repo, task);

    let report = json!({
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
            "context_files": ["file:escaping.txt"],
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
            artifact("already-landed.json", report),
            artifact("validation.json", log),
        ],
    );

    let err = action(&host, &commit_input(&repo, &head))
        .expect_err("anchor symlink escaping workspace must be refused");
    assert!(
        err.to_string()
            .contains("scope anchor is outside the tested workspace"),
        "expected outside workspace refusal, got: {err}"
    );
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

/// A clean tree at the pinned HEAD with neither route artifact must name both
/// routes, so a task that correctly changed nothing is pointed at no-diff.json
/// [F2026-10-169]. The reason keeps its already_landed_unverified prefix.
#[test]
fn commit_verifier_clean_tree_without_evidence_names_both_routes() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());

    let message = action(&host, &commit_input(temp.path(), &head))
        .expect_err("a clean tree without evidence must be refused")
        .to_string();
    for expected in [
        "already_landed_unverified:",
        "no-diff.json",
        "already-landed.json",
    ] {
        assert!(
            message.contains(expected),
            "refusal must name {expected} so a no-change task finds its route [F2026-10-169], got: {message}"
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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn doc_duties_fixture_task() -> Task {
    let mut task = fixture_task();
    task.task_type = TaskType::Chore;
    task.title = "Validate the oldest workspace documentation".to_string();
    task.tags = vec!["doc-duties".to_string(), "docs".to_string()];
    task.context_files = vec!["dir:.".to_string()];
    task.execution_summary =
        "Outcome: success\n\nVerified 6 documents against current evidence; no drift found."
            .to_string();
    task
}

fn pipeline_commit_step_input(pipeline_asset: &str, repo: &Path, head: &str) -> Value {
    let yaml = fs::read_to_string(
        workspace_root().join(format!("crates/orbit-core/assets/jobs/{pipeline_asset}")),
    )
    .unwrap_or_else(|error| panic!("read {pipeline_asset}: {error}"));
    let parsed: Value = serde_yaml::from_str(&yaml)
        .unwrap_or_else(|error| panic!("parse {pipeline_asset}: {error}"));
    let steps = parsed["spec"]["steps"]
        .as_array()
        .or_else(|| parsed["steps"].as_array())
        .unwrap_or_else(|| panic!("{pipeline_asset} must have steps"));
    let commit_step = steps
        .iter()
        .find(|step| step["id"] == "commit")
        .unwrap_or_else(|| panic!("{pipeline_asset} must have a commit step"));
    let default_input = &commit_step["default_input"];
    assert_eq!(
        default_input["verify_already_landed"], true,
        "{pipeline_asset} commit step must declare verify_already_landed: true [ORB-14644]"
    );

    json!({
        "scope": default_input["scope"].as_str().unwrap_or("all"),
        "job_run_id": RUN_ID,
        "workspace_path": repo.to_str().unwrap(),
        "base_ref": "refs/heads/main",
        "base_sha": head,
        "verify_already_landed": default_input["verify_already_landed"].as_bool().unwrap_or(false),
    })
}

/// A clean doc-duties chore (tagged `doc-duties` + `docs`, with `dir:.` context
/// and no `no-diff-expected` tag) succeeds through the local pipeline's commit
/// step when backed by valid no-diff evidence, without manufacturing a diff.
/// Both local and PR ship modes accept the clean tree, while missing evidence
/// remains refused as an empty stage [ORB-14644].
#[test]
fn clean_doc_duties_task_completes_local_and_pr_pipeline_commit_with_no_diff_evidence() {
    let temp = tempdir().expect("create tempdir");
    let repo = temp.path();
    init_git_repo(repo);
    let head = git_head(repo);
    let host = VerifierHost::new(repo, doc_duties_fixture_task());

    let valid_no_diff = json!({
        "schema_version": 1,
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "tested_head": head,
        "reason": "Doc-duties batch accurate; no drift detected.",
        "validation": [{
            "command": "make ci-fast",
            "exit_code": 0,
            "log_artifact": "validation.json"
        }]
    });
    let log = json!({
        "run_id": RUN_ID,
        "tested_head": head,
        "command": "make ci-fast",
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

    // 1. Clean doc-duties chore succeeds through the local pipeline's commit step
    let local_input = pipeline_commit_step_input("task_local_pipeline.yaml", repo, &head);
    let local_result = action(&host, &local_input)
        .expect("clean doc-duties chore must succeed through local pipeline commit step");
    assert_eq!(local_result["decision"], "verified_no_diff");
    assert_eq!(local_result["skipped_no_diff_expected"], true);
    assert_eq!(local_result["committed"], false);

    // 2. Clean doc-duties chore also succeeds through the PR pipeline's commit step
    let pr_input = pipeline_commit_step_input("task_pr_pipeline.yaml", repo, &head);
    let pr_result = action(&host, &pr_input)
        .expect("clean doc-duties chore must succeed through PR pipeline commit step");
    assert_eq!(pr_result["decision"], "verified_no_diff");
    assert_eq!(pr_result["skipped_no_diff_expected"], true);
    assert_eq!(pr_result["committed"], false);

    // The working tree remains clean and HEAD unchanged
    assert_eq!(git_head(repo), head);
    assert!(git_output(repo, &["status", "--porcelain", "--untracked-files=all"]).is_empty());

    // 3. Without no-diff evidence, an empty doc-duties stage is refused (not skipped)
    let empty_host = VerifierHost::new(repo, doc_duties_fixture_task());
    let error = action(&empty_host, &local_input)
        .expect_err("clean doc-duties task without no-diff evidence must be refused");
    assert!(
        error.to_string().contains("nothing to commit"),
        "expected empty stage refusal, got: {error}"
    );

    // 4. When doc-duties corrected drift, the diff is committed
    fs::write(repo.join("README.md"), "drift corrected\n").expect("write drift edit");
    let committed = action(&host, &local_input)
        .expect("doc-duties task with drift correction commits its changes");
    assert_eq!(committed["decision"], "performed");
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["skipped_no_diff_expected"], false);
    assert_ne!(git_head(repo), head);
    assert_eq!(
        fs::read_to_string(repo.join("README.md")).unwrap(),
        "drift corrected\n"
    );
}

/// Plant a nested repository at `sub/` whose own config runs a clean filter
/// that creates `marker`, as a sandboxed agent can inside its worktree.
fn plant_filtering_nested_repo(repo: &Path, marker: &Path) -> PathBuf {
    let sub = repo.join("sub");
    fs::create_dir_all(&sub).expect("create nested repo");
    let git = |args: &[&str]| git_output(&sub, args);
    git(&["init", "-q"]);
    git(&["config", "user.name", "Agent"]);
    git(&["config", "user.email", "agent@example.invalid"]);
    let hooks = sub.join(".git").join("orbit-test-empty-hooks");
    fs::create_dir_all(&hooks).expect("create empty hooks dir");
    git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
    fs::write(sub.join("f.txt"), "hi\n").expect("write nested file");
    fs::write(sub.join(".gitattributes"), "* filter=pwn\n").expect("write attributes");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "nested"]);
    git(&[
        "config",
        "filter.pwn.clean",
        &format!("touch '{}' && cat", marker.display()),
    ]);
    make_nested_stat_stale(&sub);
    sub
}

/// Give the nested file a stat its index does not record, so any status
/// inside the nested repository re-hashes it through the clean filter.
fn make_nested_stat_stale(sub: &Path) {
    fs::File::options()
        .write(true)
        .open(sub.join("f.txt"))
        .expect("open nested file")
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(978_307_200))
        .expect("set nested mtime");
}

/// ORB-15265: owner delivery refuses an agent-planted nested repository,
/// which `git add` would commit as a gitlink, and leaves the index unchanged.
/// With a gitlink already in history, delivery's host status and diff never
/// recurse into it, so the nested repository's filter never runs on the host.
#[test]
fn owner_delivery_refuses_nested_repos_and_never_runs_their_filters() {
    let temp = tempdir().expect("create tempdir");
    let repo = temp.path();
    init_git_repo(repo);
    let outside = tempdir().expect("create marker dir");
    let marker = outside.path().join("MARKER");
    let mut task = fixture_task();
    task.execution_summary = "Outcome: success\n\nEdited the readme.".to_string();
    let host = VerifierHost::new(repo, task);
    let sub = plant_filtering_nested_repo(repo, &marker);
    fs::write(repo.join("README.md"), "edited\n").expect("write README");
    let head = git_head(repo);
    let index = git_output(repo, &["ls-files", "--stage"]);
    let input = json!({
        "scope": "all",
        "job_run_id": RUN_ID,
        "workspace_path": repo,
    });

    let error = action(&host, &input)
        .expect_err("a nested repository must not be delivered as a gitlink")
        .to_string();
    assert!(
        error.contains("\"sub/\""),
        "refusal names the path: {error}"
    );
    assert_eq!(git_head(repo), head);
    assert_eq!(
        git_output(repo, &["ls-files", "--stage"]),
        index,
        "the refusal leaves the index unchanged"
    );
    assert!(!marker.exists(), "the nested filter ran during the refusal");

    // A gitlink already in history, as owner delivery committed it before
    // this refusal existed.
    git_output(repo, &["add", "--", "sub"]);
    git_output(repo, &["commit", "-q", "-m", "gitlink", "--", "sub"]);
    assert!(
        git_output(repo, &["ls-tree", "HEAD", "sub"]).starts_with("160000 "),
        "the fixture commits a gitlink"
    );
    make_nested_stat_stale(&sub);
    let delivered = action(&host, &input).expect("the tracked edit delivers");
    assert_eq!(delivered["decision"], "performed");
    assert_eq!(
        git_output(repo, &["show", "--format=", "--name-only", "HEAD"]),
        "README.md"
    );
    assert!(
        !marker.exists(),
        "host Git ran the nested repository's filter"
    );

    // Control: plain status without Orbit's host policy does run the filter,
    // so the assertions above observe a real execution path.
    make_nested_stat_stale(&sub);
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .expect("run git status");
    assert!(status.status.success());
    assert!(marker.exists(), "the fixture's filter must be reachable");
}
