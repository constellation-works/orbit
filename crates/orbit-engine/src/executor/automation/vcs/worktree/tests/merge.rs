#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::context::RuntimeHost;

use super::super::merge::merge_batch_worktree_into_base;

const BASE_BRANCH: &str = "agent-main";

#[test]
fn unpublished_first_merge_does_not_cascade_into_second_local_merge() {
    let temp = tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    let primary = temp.path().join("primary");
    let first_worktree = temp.path().join("first-worktree");
    let second_worktree = temp.path().join("second-worktree");

    git(temp.path(), &["init", "--bare", path(&remote)]);
    init_repo(&seed);
    commit_file(&seed, "base.txt", "base");
    git(&seed, &["remote", "add", "origin", path(&remote)]);
    git(&seed, &["push", "-u", "origin", BASE_BRANCH]);
    git(
        temp.path(),
        &[
            "clone",
            "--branch",
            BASE_BRANCH,
            path(&remote),
            path(&primary),
        ],
    );
    configure_identity(&primary);

    add_task_worktree(&primary, &first_worktree, "orbit/first");
    let first_commit = commit_file(&first_worktree, "first.txt", "first task");
    let host = MergeTestHost::new(&primary, temp.path());

    merge_batch_worktree_into_base(&host, &merge_input("first-run", &first_worktree, "local"))
        .unwrap();

    // A later bookkeeping checkpoint fails here: the local base contains the
    // first task, while the remote still names the original session base.
    assert!(is_ancestor(&primary, &first_commit, BASE_BRANCH));
    assert!(!is_ancestor(
        &primary,
        &first_commit,
        &format!("origin/{BASE_BRANCH}")
    ));

    add_task_worktree(&primary, &second_worktree, "orbit/second");
    let second_commit_before_rebase = commit_file(&second_worktree, "second.txt", "second task");

    let refusal = merge_batch_worktree_into_base(
        &host,
        &merge_input("second-run", &second_worktree, "remote"),
    )
    .unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("reconcile it or run with base_sync=local"),
        "remote mode must retain the safety refusal: {refusal}"
    );

    merge_batch_worktree_into_base(&host, &merge_input("second-run", &second_worktree, "local"))
        .unwrap();

    let second_commit_after_rebase = git(&second_worktree, &["rev-parse", "HEAD"]);
    assert_ne!(second_commit_after_rebase, second_commit_before_rebase);
    assert!(is_ancestor(&primary, &first_commit, BASE_BRANCH));
    assert!(is_ancestor(
        &primary,
        &second_commit_after_rebase,
        BASE_BRANCH
    ));
    assert_eq!(
        fs::read_to_string(primary.join("second.txt")).unwrap(),
        "second task"
    );
}

#[test]
fn child_pipeline_merges_into_an_epic_branch_checked_out_in_another_worktree() {
    let temp = tempdir().unwrap();
    let primary = temp.path().join("primary");
    let epic_worktree = temp.path().join("epic-worktree");
    let child_worktree = temp.path().join("child-worktree");
    init_repo(&primary);
    commit_file(&primary, "base.txt", "base");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "epic/ORB-10000",
            path(&epic_worktree),
            BASE_BRANCH,
        ],
    );
    configure_identity(&epic_worktree);
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "orbit/child",
            path(&child_worktree),
            "epic/ORB-10000",
        ],
    );
    configure_identity(&child_worktree);
    let child_commit = commit_file(&child_worktree, "child.txt", "child change");
    let host = MergeTestHost::new(&primary, temp.path());

    merge_batch_worktree_into_base(
        &host,
        &json!({
            "run_id": "child-run",
            "workspace_path": child_worktree,
            "base": "epic/ORB-10000",
            "base_sync": "local",
        }),
    )
    .unwrap();

    assert_eq!(git(&epic_worktree, &["rev-parse", "HEAD"]), child_commit);
    assert_eq!(
        fs::read_to_string(epic_worktree.join("child.txt")).unwrap(),
        "child change"
    );
    assert!(!primary.join("child.txt").exists());
}

fn merge_input(run_id: &str, workspace_path: &Path, base_sync: &str) -> Value {
    json!({
        "run_id": run_id,
        "workspace_path": workspace_path,
        "base": BASE_BRANCH,
        "base_sync": base_sync,
    })
}

fn add_task_worktree(repo: &Path, worktree: &Path, branch: &str) {
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path(worktree),
            &format!("origin/{BASE_BRANCH}"),
        ],
    );
    configure_identity(worktree);
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", BASE_BRANCH]);
    configure_identity(path);
}

fn configure_identity(repo: &Path) {
    git(repo, &["config", "user.name", "Orbit Test"]);
    git(repo, &["config", "user.email", "orbit-test@example.com"]);
}

fn commit_file(repo: &Path, file_name: &str, contents: &str) -> String {
    fs::write(repo.join(file_name), contents).unwrap();
    git(repo, &["add", file_name]);
    git(repo, &["commit", "-m", &format!("write {file_name}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo)
        .status()
        .unwrap()
        .success()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
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

struct MergeTestHost {
    repo_root: PathBuf,
    data_root: PathBuf,
    scoreboard_dir: PathBuf,
}

impl MergeTestHost {
    fn new(repo_root: &Path, root: &Path) -> Self {
        Self {
            repo_root: repo_root.to_path_buf(),
            data_root: root.join("data"),
            scoreboard_dir: root.join("scoreboard"),
        }
    }
}

impl RuntimeHost for MergeTestHost {
    fn record_event(&self, _event: OrbitEvent) -> Result<(), OrbitError> {
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo_root.to_string_lossy().to_string())
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        Ok(Vec::new())
    }

    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn run_tool_with_context_and_role(
        &self,
        _name: &str,
        _input: Value,
        _role: Role,
        _tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        Err(OrbitError::Execution(
            "tool execution is not needed by merge tests".to_string(),
        ))
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

/// `origin/<branch>` is an accepted spelling of the base everywhere else; the
/// merge must land on the local branch, not a detached remote ref.
#[test]
fn an_origin_prefixed_base_lands_on_the_local_branch() {
    let temp = tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    let primary = temp.path().join("primary");
    let worktree = temp.path().join("worktree");

    git(temp.path(), &["init", "--bare", path(&remote)]);
    init_repo(&seed);
    commit_file(&seed, "base.txt", "base");
    git(&seed, &["remote", "add", "origin", path(&remote)]);
    git(&seed, &["push", "-u", "origin", BASE_BRANCH]);
    git(
        temp.path(),
        &[
            "clone",
            "--branch",
            BASE_BRANCH,
            path(&remote),
            path(&primary),
        ],
    );
    configure_identity(&primary);
    add_task_worktree(&primary, &worktree, "orbit/prefixed");
    let commit = commit_file(&worktree, "task.txt", "task");
    let host = MergeTestHost::new(&primary, temp.path());

    let mut input = merge_input("prefixed-run", &worktree, "local");
    input["base"] = json!(format!("origin/{BASE_BRANCH}"));
    let result = merge_batch_worktree_into_base(&host, &input).unwrap();

    assert_eq!(result["base"], BASE_BRANCH);
    assert!(is_ancestor(&primary, &commit, BASE_BRANCH));
}
