use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use chrono::Utc;
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ExternalRef, Task, TaskArtifact, TaskComment, TaskPriority, TaskStatus, TaskType,
    push_external_ref_if_missing,
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use crate::context::{PrConfig, RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate};

use super::super::super::freshness::BranchFreshness;
use super::super::super::operations;

/// The intermediate branch of the stacked fixtures (ORB-10644).
pub const STACKED_BASE_BRANCH: &str = "orbit/ORB-10643";
pub const PUSH_OPERATION: &str = operations::PUSH;
pub const PR_LIST_OPERATION: &str = operations::PR_LIST;
pub const PR_CREATE_OPERATION: &str = operations::PR_CREATE;
pub const PR_VIEW_OPERATION: &str = operations::PR_VIEW;
pub const PR_MERGE_OPERATION: &str = operations::PR_MERGE;
pub const PR_MERGE_CAPABILITIES_OPERATION: &str = operations::PR_MERGE_CAPABILITIES;
pub const PR_STATUS_OPERATION: &str = operations::PR_STATUS;

#[derive(Clone, Debug)]
pub struct VcsCall {
    pub operation: String,
    pub input: Value,
}

pub struct PrOpenTestHost {
    tasks: Mutex<Vec<Task>>,
    comments: Mutex<HashMap<String, Vec<TaskComment>>>,
    vcs_calls: Mutex<Vec<VcsCall>>,
    automation_updates: Mutex<Vec<(String, TaskAutomationUpdate)>>,
    activity_implementer: Option<(String, String)>,
    repo_root: PathBuf,
    data_root: PathBuf,
    scoreboard_dir: PathBuf,
    vcs_errors: Mutex<HashMap<String, String>>,
    queued_vcs_results: Mutex<HashMap<String, VecDeque<Result<Value, String>>>>,
    pr_exists: Mutex<bool>,
    activity_updates: Mutex<Vec<(String, TaskActivityUpdate)>>,
}

impl PrOpenTestHost {
    pub fn new(tasks: Vec<Task>, repo_root: PathBuf) -> Self {
        let data_root = repo_root.join(".orbit-test-data");
        let scoreboard_dir = data_root.join("scoreboard");
        Self {
            tasks: Mutex::new(tasks),
            comments: Mutex::new(HashMap::new()),
            vcs_calls: Mutex::new(Vec::new()),
            automation_updates: Mutex::new(Vec::new()),
            activity_implementer: None,
            repo_root,
            data_root,
            scoreboard_dir,
            vcs_errors: Mutex::new(HashMap::new()),
            queued_vcs_results: Mutex::new(HashMap::new()),
            pr_exists: Mutex::new(false),
            activity_updates: Mutex::new(Vec::new()),
        }
    }

    pub fn with_activity_implementer(mut self, agent: &str, model: &str) -> Self {
        self.activity_implementer = Some((agent.to_string(), model.to_string()));
        self
    }

    pub fn fail_vcs(&self, operation: &str, message: &str) {
        self.vcs_errors
            .lock()
            .expect("VCS errors lock")
            .insert(operation.to_string(), message.to_string());
    }

    pub fn queue_vcs_error(&self, operation: &str, message: &str) {
        self.queued_vcs_results
            .lock()
            .expect("queued VCS results lock")
            .entry(operation.to_string())
            .or_default()
            .push_back(Err(message.to_string()));
    }

    pub fn queue_vcs_result(&self, operation: &str, result: Value) {
        self.queued_vcs_results
            .lock()
            .expect("queued VCS results lock")
            .entry(operation.to_string())
            .or_default()
            .push_back(Ok(result));
    }

    pub fn with_existing_pr(self) -> Self {
        *self.pr_exists.lock().expect("pr exists lock") = true;
        self
    }

    pub fn comments_for(&self, task_id: &str) -> Vec<TaskComment> {
        self.comments
            .lock()
            .expect("comments lock")
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn vcs_calls(&self) -> Vec<VcsCall> {
        self.vcs_calls.lock().expect("VCS calls lock").clone()
    }

    pub fn pr_create_body(&self) -> String {
        self.vcs_calls()
            .into_iter()
            .find(|call| call.operation == operations::PR_CREATE)
            .and_then(|call| {
                call.input
                    .get("body")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .expect("private PR create body")
    }

    /// Queue the sequence of `pr.status` answers a completion poll will read.
    ///
    /// Each entry is one `gh pr view` response, so a test spells out the exact
    /// state machine GitHub walks through — no live API and no wall clock.
    pub fn queue_pr_status(&self, states: impl IntoIterator<Item = Value>) {
        for state in states {
            self.queue_vcs_result(operations::PR_STATUS, json!({ "pull_request": state }));
        }
    }

    pub fn queue_merge_capabilities(
        &self,
        squash: bool,
        rebase: bool,
        merge: bool,
        requires_linear_history: bool,
    ) {
        self.queue_merge_capabilities_with_auto_merge(
            squash,
            rebase,
            merge,
            requires_linear_history,
            true,
        );
    }

    pub fn queue_merge_capabilities_with_auto_merge(
        &self,
        squash: bool,
        rebase: bool,
        merge: bool,
        requires_linear_history: bool,
        allow_auto_merge: bool,
    ) {
        self.queue_vcs_result(
            operations::PR_MERGE_CAPABILITIES,
            json!({
                "repository": {
                    "name_with_owner": "orbit/test",
                    "base_branch": "agent-main",
                    "allow_squash_merge": squash,
                    "allow_rebase_merge": rebase,
                    "allow_merge_commit": merge,
                    "allow_auto_merge": allow_auto_merge,
                    "requires_linear_history": requires_linear_history,
                }
            }),
        );
    }

    pub fn activity_updates(&self) -> Vec<(String, TaskActivityUpdate)> {
        self.activity_updates
            .lock()
            .expect("activity updates lock")
            .clone()
    }

    pub fn task_status(&self, task_id: &str) -> TaskStatus {
        self.tasks
            .lock()
            .expect("tasks lock")
            .iter()
            .find(|task| task.id == task_id)
            .map(|task| task.status)
            .unwrap_or_else(|| panic!("task {task_id} exists"))
    }

    pub fn automation_updates(&self) -> Vec<(String, TaskAutomationUpdate)> {
        self.automation_updates
            .lock()
            .expect("automation updates lock")
            .clone()
    }
}

impl RuntimeHost for PrOpenTestHost {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .lock()
            .expect("tasks lock")
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }

    fn get_task_artifacts(&self, _task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(Vec::new())
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        Ok(self
            .comments
            .lock()
            .expect("comments lock")
            .get(task_id)
            .cloned()
            .unwrap_or_default())
    }

    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        batch_id: Option<&str>,
        external_ref: Option<&orbit_types::task::ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        Ok(self
            .tasks
            .lock()
            .expect("tasks lock")
            .iter()
            .filter(|task| status.is_none_or(|status| task.status == status))
            .filter(|task| priority.is_none_or(|priority| task.priority == priority))
            .filter(|task| parent_id.is_none_or(|parent_id| task.parent_id() == Some(parent_id)))
            .filter(|task| {
                batch_id.is_none_or(|batch_id| task.job_run_id.as_deref() == Some(batch_id))
            })
            .filter(|task| {
                external_ref.is_none_or(|external_ref| {
                    task.external_refs.iter().any(|candidate| {
                        candidate.system == external_ref.system && candidate.id == external_ref.id
                    })
                })
            })
            .filter(|task| {
                has_external_ref_system.is_none_or(|system| {
                    task.external_refs
                        .iter()
                        .any(|candidate| candidate.system == system)
                })
            })
            .cloned()
            .collect())
    }

    fn start_task(
        &self,
        _task_id: &str,
        _note: Option<String>,
        _comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        Err(OrbitError::Execution(
            "start_task is not needed by pr_open tests".to_string(),
        ))
    }

    fn admit_task_for_workflow(&self, _task_id: &str, _workflow: &str) -> Result<Task, OrbitError> {
        Err(OrbitError::Execution(
            "admit_task_for_workflow is not needed by pr_open tests".to_string(),
        ))
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        self.activity_updates
            .lock()
            .expect("activity updates lock")
            .push((task_id.to_string(), update.clone()));

        let mut tasks = self.tasks.lock().expect("tasks lock");
        let task = tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        task.status = update.status;
        if let Some(model) = update.model.clone().or(update.agent.clone()) {
            task.implemented_by = Some(model);
        }
        Ok(task.clone())
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        self.automation_updates
            .lock()
            .expect("automation updates lock")
            .push((task_id.to_string(), update.clone()));

        if !update.append_comments.is_empty() {
            self.comments
                .lock()
                .expect("comments lock")
                .entry(task_id.to_string())
                .or_default()
                .extend(update.append_comments.iter().cloned());
        }

        let mut tasks = self.tasks.lock().expect("tasks lock");
        let task = tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        let transition_implemented_by =
            if matches!(update.status, Some(TaskStatus::Review | TaskStatus::Done)) {
                Some(
                    update
                        .model
                        .clone()
                        .or(update.agent.clone())
                        .unwrap_or_else(|| "system".to_string()),
                )
            } else {
                None
            };
        if let Some(status) = update.status {
            task.status = status;
        }
        for external_ref in update.external_refs {
            push_external_ref_if_missing(&mut task.external_refs, external_ref);
        }
        if let Some(execution_summary) = update.execution_summary {
            task.execution_summary = execution_summary;
        }
        if let Some(implemented_by) = transition_implemented_by {
            task.implemented_by = Some(implemented_by);
        }
        Ok(())
    }

    fn record_event(&self, _event: OrbitEvent) -> Result<(), OrbitError> {
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo_root.to_string_lossy().to_string())
    }

    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn activity_implementer_identity(
        &self,
        _input: &Value,
    ) -> Result<(Option<String>, Option<String>), OrbitError> {
        Ok(self
            .activity_implementer
            .clone()
            .map(|(agent, model)| (Some(agent), Some(model)))
            .unwrap_or((None, None)))
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        self.vcs_calls
            .lock()
            .expect("VCS calls lock")
            .push(VcsCall {
                operation: operation.to_string(),
                input: input.clone(),
            });

        if let Some(message) = self
            .vcs_errors
            .lock()
            .expect("VCS errors lock")
            .get(operation)
            .cloned()
        {
            return Err(OrbitError::Execution(message));
        }
        if let Some(result) = self
            .queued_vcs_results
            .lock()
            .expect("queued VCS results lock")
            .get_mut(operation)
            .and_then(VecDeque::pop_front)
        {
            return result.map_err(OrbitError::Execution);
        }

        match operation {
            operations::PUSH => Ok(json!({})),
            operations::PR_MERGE => Ok(json!({})),
            operations::PR_MERGE_CAPABILITIES => Ok(json!({
                "repository": {
                    "name_with_owner": "orbit/test",
                    "base_branch": "agent-main",
                    "allow_squash_merge": true,
                    "allow_rebase_merge": true,
                    "allow_merge_commit": true,
                    "allow_auto_merge": true,
                    "requires_linear_history": true,
                }
            })),
            operations::PR_LIST => {
                let head = input.get("head").and_then(Value::as_str).ok_or_else(|| {
                    OrbitError::InvalidInput("private PR list requires a head branch".to_string())
                })?;
                let pull_requests = if *self.pr_exists.lock().expect("pr exists lock") {
                    json!([{
                        "number": 42,
                        "headRefName": head,
                    }])
                } else {
                    json!([])
                };
                Ok(json!({ "pull_requests": pull_requests }))
            }
            operations::PR_CREATE => {
                *self.pr_exists.lock().expect("pr exists lock") = true;
                Ok(json!({
                    "url": "https://github.example/orbit/orbit/pull/42"
                }))
            }
            operations::PR_VIEW => {
                let selector = input.get("pr").and_then(Value::as_str).ok_or_else(|| {
                    OrbitError::InvalidInput("private PR view requires a PR selector".to_string())
                })?;
                let valid_selector = selector.chars().all(|character| character.is_ascii_digit())
                    || (selector.contains("://") && selector.contains("/pull/"));
                if !valid_selector {
                    return Err(OrbitError::InvalidInput(format!(
                        "invalid pr: {selector}; must be a numeric PR number or GitHub PR URL"
                    )));
                }
                if *self.pr_exists.lock().expect("pr exists lock") {
                    Ok(json!({
                        "pull_request": {
                            "number": 42,
                            "url": "https://github.example/orbit/orbit/pull/42"
                        }
                    }))
                } else {
                    Err(OrbitError::Execution("no pull request found".to_string()))
                }
            }
            other => Err(OrbitError::InvalidInput(format!(
                "unknown private automation VCS operation '{other}'"
            ))),
        }
    }

    fn maybe_create_failure_task(
        &self,
        _job_id: &str,
        _run_id: &str,
        _error_code: &str,
        _error_message: &str,
        _agent: Option<&str>,
        _model: Option<&str>,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn scoring_enabled(&self) -> bool {
        false
    }

    fn scoreboard_dir(&self) -> &Path {
        &self.scoreboard_dir
    }
}

pub fn task(id: &str, title: &str, execution_summary: &str) -> Task {
    let now = Utc::now();
    Task {
        id: id.to_string(),
        title: title.to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: execution_summary.to_string(),
        context_files: Vec::new(),
        created_by: Some(TEST_CODEX_MODEL.to_string()),
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::Review,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}

pub fn batch_task(id: &str, title: &str, execution_summary: &str) -> Task {
    let mut task = task(id, title, execution_summary);
    task.status = TaskStatus::InProgress;
    task.job_run_id = Some("batch-1".to_string());
    task
}

pub fn review_batch_task(id: &str, implemented_by: Option<&str>, created_by: Option<&str>) -> Task {
    let mut task = task(
        id,
        "Ship attribution",
        "Outcome: success\n\nChanges:\n- Ready.",
    );
    task.status = TaskStatus::Review;
    task.pr_status = Some("approved".to_string());
    task.job_run_id = Some("batch-1".to_string());
    task.implemented_by = implemented_by.map(ToOwned::to_owned);
    task.created_by = created_by.map(ToOwned::to_owned);
    task.external_refs = vec![ExternalRef::github_pr("42").expect("github pr ref")];
    task
}

pub fn task_with_contract(
    id: &str,
    title: &str,
    execution_summary: &str,
    description: &str,
    acceptance_criteria: &[String],
    task_url: Option<&str>,
) -> Task {
    let mut task = task(id, title, execution_summary);
    task.description = description.to_string();
    task.acceptance_criteria = acceptance_criteria.to_vec();
    if let Some(task_url) = task_url {
        task.external_refs = vec![
            ExternalRef::try_new(
                "orbit-task".to_string(),
                id.to_string(),
                Some(task_url.to_string()),
            )
            .expect("task url external ref"),
        ];
    }
    task
}

pub fn freshness() -> BranchFreshness {
    BranchFreshness {
        base_ref: "main".to_string(),
        head_ref: "feature/task".to_string(),
        commits_behind: 0,
        commits_ahead: 2,
    }
}

pub fn test_pr_config(task_url_template: Option<&str>) -> PrConfig {
    PrConfig {
        task_url_template: task_url_template.map(ToOwned::to_owned),
    }
}

pub struct PrWorkspace {
    _temp: TempDir,
    pub repo: PathBuf,
}

pub fn pr_workspace() -> PrWorkspace {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote.git");
    git(
        temp.path(),
        &["init", "--bare", remote.to_str().expect("remote path")],
    );
    fs::create_dir_all(&repo).expect("create repo dir");
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", "agent-main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    fs::write(repo.join("README.md"), "base\n").expect("write readme");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "base"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    git(&repo, &["push", "-u", "origin", "agent-main"]);
    git(&repo, &["checkout", "-b", "orbit/test-batch"]);
    fs::create_dir_all(repo.join("src")).expect("create src dir");
    fs::write(repo.join("src/lib.rs"), "pub fn changed() {}\n").expect("write lib");
    git(&repo, &["add", "src/lib.rs"]);
    git(&repo, &["commit", "-m", "change"]);
    git(&repo, &["push", "-u", "origin", "orbit/test-batch"]);

    PrWorkspace { _temp: temp, repo }
}

pub fn no_diff_pr_workspace() -> PrWorkspace {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote.git");
    git(
        temp.path(),
        &["init", "--bare", remote.to_str().expect("remote path")],
    );
    fs::create_dir_all(&repo).expect("create repo dir");
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", "agent-main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    fs::write(repo.join("README.md"), "base\n").expect("write readme");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "base"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    git(&repo, &["push", "-u", "origin", "agent-main"]);
    git(&repo, &["checkout", "-b", "orbit/test-batch"]);

    PrWorkspace { _temp: temp, repo }
}

/// Workspace where rebasing `orbit/test-batch` onto `agent-main` conflicts.
/// Recovery can resolve and continue the still-active rebase before the
/// checkpointed rebase activity is retried.
pub fn rebase_conflict_pr_workspace() -> PrWorkspace {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote.git");
    git(
        temp.path(),
        &["init", "--bare", remote.to_str().expect("remote path")],
    );
    fs::create_dir_all(&repo).expect("create repo dir");
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", "agent-main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    fs::write(repo.join("README.md"), "base\n").expect("write readme");
    fs::create_dir_all(repo.join("src")).expect("create src dir");
    fs::write(repo.join("src/lib.rs"), "pub fn base() {}\n").expect("write lib");
    git(&repo, &["add", "README.md", "src/lib.rs"]);
    git(&repo, &["commit", "-m", "base"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    git(&repo, &["checkout", "-b", "orbit/test-batch"]);
    fs::write(repo.join("src/lib.rs"), "pub fn branch() {}\n").expect("write branch lib");
    git(&repo, &["add", "src/lib.rs"]);
    git(&repo, &["commit", "-m", "branch change"]);
    git(&repo, &["push", "-u", "origin", "orbit/test-batch"]);
    git(&repo, &["checkout", "agent-main"]);
    fs::write(repo.join("src/lib.rs"), "pub fn diverged() {}\n").expect("write base lib");
    git(&repo, &["add", "src/lib.rs"]);
    git(&repo, &["commit", "-m", "diverged base"]);
    git(&repo, &["push", "-u", "origin", "agent-main"]);
    git(&repo, &["checkout", "orbit/test-batch"]);

    PrWorkspace { _temp: temp, repo }
}

/// The stacked shape ORB-10644 is about: `orbit/test-batch` is cut from the
/// intermediate branch `STACKED_BASE_BRANCH`, which is itself cut from the
/// landing branch `agent-main`. All three exist on `origin`, so the base is a
/// live delivery target until `land_stacked_base_by_squash` lands it.
pub fn stacked_pr_workspace() -> PrWorkspace {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote.git");
    git(
        temp.path(),
        &["init", "--bare", remote.to_str().expect("remote path")],
    );
    fs::create_dir_all(&repo).expect("create repo dir");
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", "agent-main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    // A machine-global `core.hooksPath` rewrites fixture commit messages
    // (ORB-10350); the delivery markers this fixture is grepped for must be
    // exactly what it wrote.
    let hooks = repo.join(".git").join("orbit-test-empty-hooks");
    fs::create_dir_all(&hooks).expect("create empty hooks dir");
    git(
        &repo,
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    );
    fs::write(repo.join("README.md"), "base\n").expect("write readme");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "[ORB-10600] base"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    git(&repo, &["push", "-u", "origin", "agent-main"]);

    git(&repo, &["checkout", "-b", STACKED_BASE_BRANCH]);
    fs::write(repo.join("parent.txt"), "parent\n").expect("write parent");
    git(&repo, &["add", "parent.txt"]);
    git(&repo, &["commit", "-m", "[ORB-10643] parent work"]);
    git(&repo, &["push", "-u", "origin", STACKED_BASE_BRANCH]);

    git(&repo, &["checkout", "-b", "orbit/test-batch"]);
    fs::create_dir_all(repo.join("src")).expect("create src dir");
    fs::write(repo.join("src/lib.rs"), "pub fn changed() {}\n").expect("write lib");
    git(&repo, &["add", "src/lib.rs"]);
    git(&repo, &["commit", "-m", "[ORB-10644] child work"]);
    git(&repo, &["push", "-u", "origin", "orbit/test-batch"]);

    PrWorkspace { _temp: temp, repo }
}

/// The intermediate branch's own PR squash-merges into `agent-main` — Orbit's
/// own merge strategy — and the branch is left behind on `origin` at its
/// pre-merge tip, exactly as a restored or never-pruned branch is. Nothing
/// about `STACKED_BASE_BRANCH` stops resolving; it simply stops leading
/// anywhere.
pub fn land_stacked_base_by_squash(repo: &Path) {
    let head = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(repo, &["checkout", "agent-main"]);
    git(repo, &["merge", "--squash", STACKED_BASE_BRANCH]);
    git(repo, &["commit", "-m", "[ORB-10643] parent work (#901)"]);
    git(repo, &["push", "origin", "agent-main"]);
    git(repo, &["checkout", &head]);
}

pub fn stacked_pr_open_input(repo: &Path, completed_task_ids: Vec<&str>) -> Value {
    let base_sha = git(repo, &["rev-parse", STACKED_BASE_BRANCH]);
    json!({
        "workspace_path": repo.to_string_lossy(),
        "job_run_id": "batch-1",
        "completed_task_ids": completed_task_ids,
        "base": STACKED_BASE_BRANCH,
        "base_ref": STACKED_BASE_BRANCH,
        "base_sha": base_sha,
        "head": "orbit/test-batch",
        "base_sync": "local",
        "landing_branch": "agent-main",
    })
}

/// The promote step's input, carrying the same base and landing declaration the
/// open step was given.
pub fn promote_input_from(open_input: &Value, pr_number: &Value, pr_url: &Value) -> Value {
    let mut input = open_input.clone();
    input["pr_number"] = pr_number.clone();
    input["pr_url"] = pr_url.clone();
    input
}

pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo)
        .output()
        .expect("run git merge-base")
        .status
        .success()
}

pub fn pr_open_input(repo: &Path, completed_task_ids: Vec<&str>) -> Value {
    let base_sha = git(repo, &["rev-parse", "agent-main"]);
    json!({
        "workspace_path": repo.to_string_lossy(),
        "job_run_id": "batch-1",
        "completed_task_ids": completed_task_ids,
        "base": "agent-main",
        "base_ref": "agent-main",
        "base_sha": base_sha,
        "head": "orbit/test-batch",
        "base_sync": "local",
    })
}

pub fn merge_batch_pr_input(repo: &Path) -> Value {
    json!({
        "workspace_path": repo.to_string_lossy(),
        "job_run_id": "batch-1",
        "base": "agent-main",
        "base_sync": "local",
    })
}

pub fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
