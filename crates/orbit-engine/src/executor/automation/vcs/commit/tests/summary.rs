//! ORB-10603 regression: delivery no longer wedges when the implementing agent
//! persisted no execution summary. The commit step derives one from the change
//! it is about to deliver, persists it to the task record, and only then meets
//! the unchanged delivery gate. An agent-authored summary is never overwritten.

use std::fs;
use std::path::Path;

use orbit_types::task::Task;
use serde_json::{Value, json};

use super::super::git_commit;
use super::super::summary::{
    MAX_LISTED_FILES, WorktreeChange, parse_status_entries, render_derived_summary,
};
use super::test_support::*;

use super::super::super::git::{git_output, git_success};

const DERIVED_TASK_ID: &str = "ORB-10603-DERIVE";

fn task_with_summary(summary: &str) -> Task {
    let mut task = task_with_file(
        DERIVED_TASK_ID,
        "Deliver work the agent never summarized",
        "src/change.txt",
        "codex",
    );
    task.execution_summary = summary.to_string();
    task
}

fn batch_input(workspace: &Path) -> Value {
    json!({
        "scope": "all",
        "job_run_id": "batch-1",
        "workspace_path": workspace.to_string_lossy().to_string(),
    })
}

/// Put an added file and a modified file in the worktree, matching what an
/// implement step leaves behind.
fn stage_worktree_change(workspace: &Path) {
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/change.txt"), "delivered\n").unwrap();
    fs::write(workspace.join("README.md"), "base\nedited\n").unwrap();
    git_success(workspace, &["add", "--", "src/change.txt"]).unwrap();
}

/// The wedge this task fixes: implementation completed, the agent wrote no
/// summary, and delivery must proceed on a derived one.
#[test]
fn commit_batch_derives_summary_when_agent_persisted_none() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    stage_worktree_change(workspace);

    let host = CommitTestHost::new(vec![task_with_summary("")], workspace.to_path_buf());
    let result = git_commit(&host, &batch_input(workspace))
        .expect("a derived summary must satisfy the delivery gate");

    assert_eq!(result["committed"], json!(true));
    assert_eq!(
        git_output(workspace, &["rev-list", "--count", "HEAD"])
            .expect("commit count")
            .trim(),
        "2",
        "the delivery commit was created"
    );

    let persisted = host.persisted_summaries();
    assert_eq!(
        persisted.len(),
        1,
        "exactly one derived summary is persisted: {persisted:?}"
    );
    let (task_id, summary) = &persisted[0];
    assert_eq!(task_id, DERIVED_TASK_ID);
    assert!(
        summary.contains("Changed files (2):"),
        "summary counts the delivered change: {summary}"
    );
    assert!(
        summary.contains("- added: src/change.txt"),
        "summary names the added file: {summary}"
    );
    assert!(
        summary.contains("- modified: README.md"),
        "summary names the modified file: {summary}"
    );
    assert!(
        summary.contains("batch-1"),
        "summary attributes the delivering run: {summary}"
    );
}

/// A placeholder reads as no summary at all, so it is derived over rather than
/// carried into the PR body.
#[test]
fn commit_batch_derives_summary_over_a_placeholder() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    stage_worktree_change(workspace);

    let host = CommitTestHost::new(vec![task_with_summary("TBD")], workspace.to_path_buf());
    git_commit(&host, &batch_input(workspace)).expect("placeholder summaries are derived over");

    let persisted = host.persisted_summaries();
    assert_eq!(persisted.len(), 1, "{persisted:?}");
    assert!(persisted[0].1.contains("- added: src/change.txt"));
}

/// An agent-authored summary is the better artifact and is left alone.
#[test]
fn commit_batch_preserves_an_agent_authored_summary() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    stage_worktree_change(workspace);

    let authored = "Outcome: success\n\nChanges:\n- Reworked the delivery gate.";
    let host = CommitTestHost::new(vec![task_with_summary(authored)], workspace.to_path_buf());
    git_commit(&host, &batch_input(workspace)).expect("an authored summary delivers unchanged");

    assert!(
        host.persisted_summaries().is_empty(),
        "no derived summary overwrites the agent's own"
    );
    assert_eq!(
        host.task_execution_summary(DERIVED_TASK_ID),
        authored,
        "durable state still holds the agent's summary"
    );
}

/// The gate is not relaxed: with no change to describe there is nothing to
/// derive, and the empty summary still refuses delivery without touching Git.
#[test]
fn commit_batch_still_blocks_when_no_change_can_be_derived() {
    let temp = initialized_git_repo();
    let workspace = temp.path();

    let head_before = git_output(workspace, &["rev-parse", "HEAD"]).expect("HEAD before");
    let host = CommitTestHost::new(vec![task_with_summary("   \n")], workspace.to_path_buf());
    let error = git_commit(&host, &batch_input(workspace))
        .expect_err("an underivable empty summary still blocks delivery");
    let message = error.to_string();
    assert!(
        message.contains(DERIVED_TASK_ID)
            && message.contains("meaningful persisted execution_summary"),
        "the unchanged delivery gate produced the error: {message}"
    );

    assert!(
        host.persisted_summaries().is_empty(),
        "nothing derivable means nothing persisted"
    );
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("HEAD after"),
        head_before,
        "the blocked delivery created no commit"
    );
    assert!(
        git_output(workspace, &["diff", "--cached", "--name-only"])
            .expect("staged files after")
            .trim()
            .is_empty(),
        "the blocked delivery staged nothing"
    );
}

#[test]
fn status_parsing_attaches_rename_sources_to_their_record() {
    let raw = "R  new/path.rs\0old/path.rs\0 M src/edited.rs\0?? src/new.rs\0D  gone.rs\0";
    let changes = parse_status_entries(raw);
    let rendered: Vec<String> = changes
        .iter()
        .map(|change| format!("{}: {}", change.kind, change.path))
        .collect();
    assert_eq!(
        rendered,
        vec![
            "deleted: gone.rs".to_string(),
            "renamed: new/path.rs".to_string(),
            "modified: src/edited.rs".to_string(),
            "added: src/new.rs".to_string(),
        ],
        "the rename source is not read as a separate change"
    );
}

#[test]
fn derived_summary_truncates_long_file_lists() {
    let changes: Vec<WorktreeChange> = (0..MAX_LISTED_FILES + 3)
        .map(|index| WorktreeChange {
            kind: "modified",
            path: format!("src/file_{index:03}.rs"),
        })
        .collect();
    let summary = render_derived_summary(DERIVED_TASK_ID, "batch-1", &changes);
    assert!(
        summary.contains(&format!("Changed files ({}):", MAX_LISTED_FILES + 3)),
        "{summary}"
    );
    assert!(summary.contains("- ... and 3 more file(s)"), "{summary}");
}
