//! The shipped local pipeline's `merge` -> `mark_review` -> `push` ->
//! `complete_tasks` tail against real Git repositories: under
//! `completion: done` the merged base is published before any task is
//! completed, a remote other hosts moved is merged in without rewriting the
//! landed commits, and a publication that cannot succeed leaves the task in
//! `review` with the reason [ORB-25144].

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
use orbit_engine::{
    DispatchError, JobOutcome, execute_job_with_resume, resolve_job_catalog_refs_for_execution,
};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::activity_job::{ActivityV2, ActivityV2Spec, DeterministicSpec};
use serde_json::{Value, json};

use super::git_fixture::{self, run as git};
use super::v2_runtime::{RECOVERY, build_writer_and_sinks, job_asset, workspace_root};

const RUN: &str = "local-push-run";

#[test]
fn completion_publishes_the_merged_base_when_the_remote_is_level() {
    git_fixture::isolated(
        module_path!(),
        "completion_publishes_the_merged_base_when_the_remote_is_level",
        || {
            let fx = Fixture::new("task.txt", "task\n");
            let outcome = fx.run("done", false);
            assert!(outcome.success, "{outcome:?}");
            let landed = fx.local("main");
            assert_eq!(fx.remote("main"), landed, "the merged base is published");
            assert_eq!(landed, fx.task_commit, "a level remote only fast-forwards");
            assert_eq!(fx.host.status(), TaskStatus::Done);
            assert!(fx.host.completed());
        },
    );
}

#[test]
fn completion_merges_a_moved_remote_without_rewriting_the_landed_commit() {
    git_fixture::isolated(
        module_path!(),
        "completion_merges_a_moved_remote_without_rewriting_the_landed_commit",
        || {
            let fx = Fixture::new("task.txt", "task\n");
            let moved = fx.push_from_another_host("other.txt", "other\n");
            let outcome = fx.run("done", false);
            assert!(outcome.success, "{outcome:?}");
            let published = fx.remote("main");
            assert_eq!(fx.local("main"), published);
            for kept in [&fx.task_commit, &moved] {
                git(
                    &fx.remote_path,
                    &["merge-base", "--is-ancestor", kept, &published],
                );
            }
            assert_ne!(published, fx.task_commit, "the remote's own commit is kept");
            assert_eq!(fx.host.status(), TaskStatus::Done);
            assert!(fx.host.completed());
        },
    );
}

#[test]
fn completion_stays_in_review_when_the_moved_remote_cannot_be_merged() {
    git_fixture::isolated(
        module_path!(),
        "completion_stays_in_review_when_the_moved_remote_cannot_be_merged",
        || {
            let fx = Fixture::new("README.md", "task\n");
            let moved = fx.push_from_another_host("README.md", "other\n");
            let failed = fx.try_run("done", false);
            let message = format!("{failed:?}");
            assert!(
                failed.as_ref().map_or(true, |outcome| !outcome.success),
                "{message}"
            );
            assert!(message.contains("push_integration_failed"), "{message}");
            assert_eq!(fx.remote("main"), moved, "nothing was published");
            assert_eq!(
                fx.local("main"),
                fx.task_commit,
                "the local base is unchanged"
            );
            assert!(
                git(&fx.repo, &["status", "--porcelain"]).is_empty(),
                "the aborted merge leaves the base checkout clean"
            );
            assert_eq!(fx.host.status(), TaskStatus::Review);
            assert!(!fx.host.completed(), "completion is never reached");
        },
    );
}

#[test]
fn a_review_ending_run_without_auto_push_publishes_nothing() {
    git_fixture::isolated(
        module_path!(),
        "a_review_ending_run_without_auto_push_publishes_nothing",
        || {
            let fx = Fixture::new("task.txt", "task\n");
            let before = fx.remote("main");
            let outcome = fx.run("review", false);
            assert!(outcome.success, "{outcome:?}");
            assert_eq!(fx.remote("main"), before);
            assert_eq!(fx.local("main"), fx.task_commit);
            assert_eq!(fx.host.status(), TaskStatus::Review);
        },
    );
}

struct Fixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
    remote_path: PathBuf,
    other: PathBuf,
    task_commit: String,
    host: Host,
}

impl Fixture {
    /// A base checkout on `main` published to a bare remote, plus a task
    /// worktree whose one commit writes `file`.
    fn new(file: &str, contents: &str) -> Self {
        let root = tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir()).unwrap();
        let (repo, remote_path, other, worktree) = (
            root.path().join("repo"),
            root.path().join("remote.git"),
            root.path().join("other"),
            root.path().join("task"),
        );
        std::fs::create_dir(&repo).unwrap();
        git_fixture::init(&repo);
        configure_identity(&repo);
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["branch", "-M", "main"]);
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--template=",
                remote_path.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &["remote", "add", "origin", remote_path.to_str().unwrap()],
        );
        git(&repo, &["push", "origin", "main"]);
        git(
            root.path(),
            &[
                "clone",
                "--branch",
                "main",
                remote_path.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        configure_identity(&other);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "task-branch",
                worktree.to_str().unwrap(),
            ],
        );
        std::fs::write(worktree.join(file), contents).unwrap();
        git(&worktree, &["add", file]);
        git(&worktree, &["commit", "-m", "task change"]);
        let task_commit = git(&worktree, &["rev-parse", "HEAD"]);
        let host = Host::new(&repo, &worktree);
        Self {
            _root: root,
            repo,
            remote_path,
            other,
            task_commit,
            host,
        }
    }

    /// Another host publishes a commit to the remote base.
    fn push_from_another_host(&self, file: &str, contents: &str) -> String {
        std::fs::write(self.other.join(file), contents).unwrap();
        git(&self.other, &["add", file]);
        git(&self.other, &["commit", "-m", "from another host"]);
        git(&self.other, &["push", "origin", "main"]);
        git(&self.other, &["rev-parse", "HEAD"])
    }

    fn local(&self, branch: &str) -> String {
        git(&self.repo, &["rev-parse", branch])
    }

    fn remote(&self, branch: &str) -> String {
        git(&self.remote_path, &["rev-parse", branch])
    }

    fn try_run(&self, completion: &str, auto_push: bool) -> Result<JobOutcome, DispatchError> {
        let audit_root = tempfile::tempdir().expect("audit tempdir");
        let (writer, _envelope, _inner) = build_writer_and_sinks(audit_root.path(), RUN);
        execute_job_with_resume(
            &shipped_tail_job(),
            json!({
                "task_ids": ["FIXTURE"],
                "base_branch": "main",
                "base_sync": "local",
                "completion": completion,
                "auto_push": auto_push,
                "terminal_status": "review",
            }),
            RUN,
            writer,
            &self.host,
            None,
        )
    }

    fn run(&self, completion: &str, auto_push: bool) -> JobOutcome {
        self.try_run(completion, auto_push)
            .expect("the local pipeline tail runs")
    }
}

fn configure_identity(repo: &Path) {
    git(repo, &["config", "user.name", "Orbit Test"]);
    git(
        repo,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
}

/// The shipped local pipeline's `merge`, `mark_review`, `push` and
/// `complete_tasks` steps; `worktree`, `commit` and the task completion write
/// are stubs so the run needs no task store.
fn shipped_tail_job() -> orbit_types::workflow::JobV2 {
    let shipped = std::fs::read_to_string(
        workspace_root().join("crates/orbit-core/assets/jobs/task_local_pipeline.yaml"),
    )
    .expect("read the shipped local pipeline");
    let shipped = load_job_asset(&shipped)
        .expect("the shipped local pipeline loads")
        .spec;
    let find_step = |id: &str| {
        shipped
            .steps
            .iter()
            .find(|step| step.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("the shipped local pipeline has `{id}`"))
    };
    let stub = |id: &str| {
        json!({
            "id": id,
            "spec": { "type": "deterministic", "action": format!("test_stub_{id}"), "config": {} },
        })
    };
    let mut job = job_asset(json!([stub("worktree"), stub("commit")]));
    job.default_input = shipped.default_input.clone();
    for id in ["merge", "mark_review", "push", "complete_tasks"] {
        job.steps.push(find_step(id));
    }
    let mut catalog = V2ActivityCatalog::new();
    for (name, action) in [
        ("git_merge", "git_merge"),
        ("update_task", "update_task"),
        ("git_push", "git_push"),
        ("task_complete", "test_stub_task_complete"),
        (RECOVERY, "test_stub_step_failure_recovery"),
    ] {
        catalog.insert(
            name.to_string(),
            ActivityV2 {
                description: format!("scripted `{name}`"),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: action.to_string(),
                    config: Value::Null,
                }),
            },
        );
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect("resolve the shipped local pipeline steps");
    job
}

/// One `in-progress` task, the real `git_merge`, `update_task` and
/// `git_push` actions and the real private VCS provider.
struct Host {
    task: Mutex<orbit_types::task::Task>,
    completed: Mutex<bool>,
    repo: PathBuf,
    worktree: PathBuf,
}

impl Host {
    fn new(repo: &Path, worktree: &Path) -> Self {
        let task: orbit_types::task::Task = serde_json::from_value(json!({
            "id": "FIXTURE",
            "title": "Fixture",
            "description": "Fixture",
            "status": "in_progress",
            "priority": "medium",
            "task_type": "bug",
            "tags": [],
            "context_files": [],
            "execution_summary": "Validated the fixture.",
            "job_run_id": RUN,
            "created_at": chrono::Utc::now(),
            "updated_at": chrono::Utc::now(),
        }))
        .expect("fixture task");
        Self {
            task: Mutex::new(task),
            completed: Mutex::new(false),
            repo: repo.to_path_buf(),
            worktree: worktree.to_path_buf(),
        }
    }

    fn status(&self) -> TaskStatus {
        self.task.lock().expect("task").status
    }

    fn completed(&self) -> bool {
        *self.completed.lock().expect("completed")
    }
}

impl orbit_engine::test_support::NullRuntimeHost for Host {
    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn get_task(&self, _: &str) -> Result<orbit_types::task::Task, OrbitError> {
        Ok(self.task.lock().expect("task").clone())
    }

    fn list_run_tasks(&self, _: &str) -> Result<Vec<orbit_types::task::Task>, OrbitError> {
        Ok(vec![self.task.lock().expect("task").clone()])
    }

    fn update_task_from_activity(
        &self,
        _: &str,
        update: orbit_engine::TaskActivityUpdate,
    ) -> Result<orbit_types::task::Task, OrbitError> {
        let mut task = self.task.lock().expect("task");
        assert_eq!(task.status, update.expected_status);
        task.status = update.status;
        Ok(task.clone())
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        orbit_engine::run_private_vcs_operation(operation, &input)
    }

    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        match action {
            "test_stub_worktree" => Ok(json!({
                "job_run_id": RUN,
                "workspace_path": self.worktree,
            })),
            "test_stub_commit" => Ok(json!({
                "skipped_no_diff_expected": false,
                "decision": "committed",
            })),
            "test_stub_step_failure_recovery" => Ok(json!({})),
            // `task_complete` is stubbed: it records the call and moves the
            // task to `done`, as the real activity does after its checks.
            "test_stub_task_complete" => {
                *self.completed.lock().expect("completed") = true;
                self.task.lock().expect("task").status = TaskStatus::Done;
                Ok(json!({}))
            }
            "git_merge" | "update_task" | "git_push" => orbit_engine::execute_deterministic_action(
                self,
                action,
                config,
                input,
                false,
                &std::collections::HashMap::new(),
                None,
            )
            .map_err(|error| DispatchError::JobExecution(error.to_string())),
            other => panic!("unexpected action `{other}`"),
        }
    }
}
