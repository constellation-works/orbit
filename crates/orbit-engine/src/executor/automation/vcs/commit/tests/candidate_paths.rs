use std::fs;
use std::path::Path;

use orbit_types::task::Task;
use serde_json::{Value, json};

use super::super::git_commit;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};
use crate::executor::automation::vcs::git::{git_output, git_success};

const CLAIMED_TASK_ID: &str = "T-CLAIMED";

/// A claimed task whose frozen footprint is `selectors`, with no exact
/// selector for any file the implementer creates.
fn claimed_task(selectors: &[&str]) -> Task {
    let mut task = task_with_file(CLAIMED_TASK_ID, "Split a module", "unused", "claude");
    task.context_files = selectors
        .iter()
        .map(|selector| (*selector).to_string())
        .collect();
    task
}

/// The `commit` step input both claimed pipelines pass, carrying this
/// attempt's implementer output.
fn claimed_commit_input(workspace: &Path) -> Value {
    json!({
        "scope": "all",
        "job_run_id": "batch-1",
        "workspace_path": workspace,
        "implementation": {
            "execution_summary": "Outcome: success\nChanges:\n- split the module",
            "context_files_added": ["file:src/split/new.rs"],
        },
    })
}

/// A fixture repo checked out on `candidate`, branched from `agent-main`, as
/// a claimed leaf's worktree is.
fn claimed_worktree() -> tempfile::TempDir {
    let temp = initialized_git_repo();
    git_success(temp.path(), &["branch", "agent-main"]).unwrap();
    git_success(temp.path(), &["checkout", "-b", "candidate"]).unwrap();
    temp
}

fn write(workspace: &Path, path: &str, contents: &str) {
    let path = workspace.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn untracked_status(workspace: &Path) -> String {
    git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .unwrap()
}

/// ORB-13756: scratch under `.orbit/tmp/` is never a delivery candidate, even
/// where the repository does not ignore it and a selector covers it: not
/// under a claim footprint admitting `dir:.orbit`, and not under a local
/// run's exact selector. It is left untouched rather than refused, since it
/// is where every worker is told to write scratch.
#[test]
fn scratch_under_orbit_tmp_is_never_delivered() {
    let claimed = |workspace: &Path| {
        CommitTestHost::new(
            vec![claimed_task(&["dir:src", "dir:.orbit"])],
            workspace.to_path_buf(),
        )
        .with_claim_binding(CLAIMED_TASK_ID)
    };
    let local = |workspace: &Path| {
        let mut task = claimed_task(&["file:src/new.rs", "file:.orbit/tmp/evidence.json"]);
        task.execution_summary = "Outcome: success\nChanges:\n- added".to_string();
        CommitTestHost::new(vec![task], workspace.to_path_buf())
    };
    let hosts: [&dyn Fn(&Path) -> CommitTestHost; 2] = [&claimed, &local];
    for host in hosts {
        let temp = claimed_worktree();
        let workspace = temp.path();
        write(workspace, "src/new.rs", "pub fn added() {}\n");
        write(workspace, ".orbit/tmp/evidence.json", "{}");
        write(workspace, ".orbit/tmp/logs/run.log", "log");

        git_commit(&host(workspace), &claimed_commit_input(workspace))
            .expect("source is delivered past the scratch");
        assert_eq!(
            git_output(workspace, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
            "src/new.rs"
        );
        assert_eq!(
            untracked_status(workspace),
            "?? .orbit/tmp/evidence.json\n?? .orbit/tmp/logs/run.log"
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".orbit/tmp/evidence.json")).unwrap(),
            "{}"
        );
    }
}
