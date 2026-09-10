use std::fs;

use serde_json::json;

use super::super::git_commit;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};
use crate::executor::automation::vcs::git::{git_output, git_success};

#[test]
fn singleton_refuses_unknown_untracked_paths_without_mutating_files_or_index() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::write(workspace.join("README.md"), "intended tracked edit\n").unwrap();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/new.rs"), "pub fn intended() {}\n").unwrap();
    git_success(workspace, &["add", "--", "src/new.rs"]).unwrap();
    fs::write(workspace.join("screenshot.png"), b"scratch screenshot").unwrap();
    fs::write(workspace.join("browser-path.txt"), b"/tmp/browser").unwrap();

    let mut task = task_with_file("T1", "Deliver source only", "README.md", "codex");
    task.context_files.clear();
    task.execution_summary = "Outcome: success\nChanges:\n- Source updated.".to_string();
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());
    let index_before = git_output(workspace, &["diff", "--cached", "--binary"]).unwrap();
    let readme_before = fs::read(workspace.join("README.md")).unwrap();
    let new_source_before = fs::read(workspace.join("src/new.rs")).unwrap();
    let screenshot_before = fs::read(workspace.join("screenshot.png")).unwrap();
    let marker_before = fs::read(workspace.join("browser-path.txt")).unwrap();
    let head_before = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();

    let error = git_commit(
        &host,
        &json!({
            "scope": "all",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
        }),
    )
    .expect_err("unknown untracked evidence must refuse delivery");
    let message = error.to_string();
    assert!(message.contains("browser-path.txt"), "{message}");
    assert!(message.contains("screenshot.png"), "{message}");
    assert!(message.contains("exact `file:` task selector"), "{message}");

    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).unwrap(),
        head_before
    );
    assert_eq!(
        git_output(workspace, &["diff", "--cached", "--binary"]).unwrap(),
        index_before
    );
    assert_eq!(
        fs::read(workspace.join("README.md")).unwrap(),
        readme_before
    );
    assert_eq!(
        fs::read(workspace.join("src/new.rs")).unwrap(),
        new_source_before
    );
    assert_eq!(
        fs::read(workspace.join("screenshot.png")).unwrap(),
        screenshot_before
    );
    assert_eq!(
        fs::read(workspace.join("browser-path.txt")).unwrap(),
        marker_before
    );
}

#[test]
fn singleton_commits_tracked_changes_and_exactly_declared_new_source() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    for (path, contents) in [
        ("delete.txt", "delete me\n"),
        ("rename-from.txt", "rename me\n"),
    ] {
        fs::write(workspace.join(path), contents).unwrap();
    }
    git_success(workspace, &["add", "--", "delete.txt", "rename-from.txt"]).unwrap();
    git_success(workspace, &["commit", "-m", "tracked fixtures"]).unwrap();

    fs::write(workspace.join("README.md"), "tracked edit\n").unwrap();
    fs::remove_file(workspace.join("delete.txt")).unwrap();
    git_success(workspace, &["mv", "--", "rename-from.txt", "rename-to.txt"]).unwrap();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/new.rs"), "pub fn added() {}\n").unwrap();

    let mut task = task_with_file("T1", "Candidate paths", "README.md", "codex");
    task.context_files.push("file:src/new.rs".to_string());
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());
    git_commit(
        &host,
        &json!({
            "scope": "all",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
        }),
    )
    .expect("the explicit candidate is committed");

    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-status", "HEAD"]).unwrap(),
        "M\tREADME.md\nD\tdelete.txt\nR100\trename-from.txt\trename-to.txt\nA\tsrc/new.rs"
    );
    assert!(
        git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn per_task_commits_only_each_tasks_explicit_candidate_paths() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::create_dir_all(workspace.join("one")).unwrap();
    fs::create_dir_all(workspace.join("two")).unwrap();
    fs::write(workspace.join("one/new.rs"), "pub fn one() {}\n").unwrap();
    fs::write(workspace.join("two/new.rs"), "pub fn two() {}\n").unwrap();

    let mut one = task_with_file("T1", "First candidate", "one/new.rs", "codex");
    one.context_files = vec!["file:one/new.rs".to_string()];
    let mut two = task_with_file("T2", "Second candidate", "two/new.rs", "claude");
    two.context_files = vec!["file:two/new.rs".to_string()];
    let host = CommitTestHost::new(vec![one, two], workspace.to_path_buf());

    let result = git_commit(
        &host,
        &json!({
            "scope": "per_task",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
            "completed_task_ids": ["T1", "T2"],
        }),
    )
    .expect("both explicitly scoped task candidates are committed");

    assert_eq!(result["committed_task_ids"], json!(["T1", "T2"]));
    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-only", "HEAD^"]).unwrap(),
        "one/new.rs"
    );
    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
        "two/new.rs"
    );
}

#[test]
fn per_task_refuses_ambiguous_or_unowned_candidates_before_index_mutation() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::create_dir_all(workspace.join("shared")).unwrap();
    fs::create_dir_all(workspace.join("orphan")).unwrap();
    fs::write(workspace.join("shared/new.rs"), "pub fn shared() {}\n").unwrap();
    fs::write(workspace.join("orphan/new.rs"), "pub fn orphan() {}\n").unwrap();
    git_success(workspace, &["add", "--", "shared/new.rs", "orphan/new.rs"]).unwrap();

    let mut one = task_with_file("T1", "First candidate", "shared", "codex");
    one.context_files = vec!["dir:shared".to_string()];
    let mut two = task_with_file("T2", "Second candidate", "shared", "claude");
    two.context_files = vec!["dir:shared".to_string()];
    let host = CommitTestHost::new(vec![one, two], workspace.to_path_buf());
    let index_before = git_output(workspace, &["diff", "--cached", "--binary"]).unwrap();

    let error = git_commit(
        &host,
        &json!({
            "scope": "per_task",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
            "completed_task_ids": ["T1", "T2"],
        }),
    )
    .expect_err("candidate ownership must be deterministic");
    let message = error.to_string();
    assert!(message.contains("orphan/new.rs"), "{message}");
    assert!(message.contains("shared/new.rs"), "{message}");
    assert!(
        message.contains("T1") && message.contains("T2"),
        "{message}"
    );
    assert_eq!(
        git_output(workspace, &["diff", "--cached", "--binary"]).unwrap(),
        index_before
    );
    assert_eq!(
        git_output(workspace, &["rev-list", "--count", "HEAD"]).unwrap(),
        "1"
    );
}
