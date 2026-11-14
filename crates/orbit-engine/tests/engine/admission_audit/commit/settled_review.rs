#![cfg(unix)]

use std::fs;
use std::path::Path;

use serde_json::json;

use super::git_commit;
use super::git_output;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};

fn git(repo: &Path, args: &[&str]) -> String {
    git_output(repo, args).expect("git command")
}

fn head(repo: &Path) -> String {
    git(repo, &["rev-parse", "HEAD"])
}

fn commit_input(repo: &Path, base_sha: &str) -> serde_json::Value {
    json!({"workspace_path": repo, "job_run_id": "batch-1", "base_sha": base_sha})
}

/// Reviewer commit at HEAD recorded by a settled before-landing review:
/// a clean tree proceeds as already committed instead of failing
/// `worktree_head_changed` (the empty-tree case is a verified no-diff).
#[test]
fn settled_review_head_proceeds_when_tree_is_clean() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::settled_review_head_proceeds_when_tree_is_clean"
    )) {
        return;
    }
    let temp = initialized_git_repo();
    let repo = temp.path();
    let base_sha = head(repo);
    git(repo, &["commit", "--allow-empty", "-qm", "reviewer repair"]);
    let reviewer_head = head(repo);

    let host = CommitTestHost::new(
        vec![task_with_file("T1", "Candidate", "README.md", "codex")],
        repo.to_path_buf(),
    )
    .with_settled_review("T1", &reviewer_head);
    let output = git_commit(&host, &commit_input(repo, &base_sha))
        .expect("settled reviewer HEAD must be accepted as the candidate");
    assert_eq!(output["decision"], json!("already_committed"));
    assert_eq!(output["committed"], json!(false));
    assert_eq!(head(repo), reviewer_head);
}

/// A resumed implementation that leaves changes on top of the settled
/// reviewer commit commits them as a child of that commit.
#[test]
fn settled_review_head_accepts_resumed_changes_on_top() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::settled_review_head_accepts_resumed_changes_on_top"
    )) {
        return;
    }
    let temp = initialized_git_repo();
    let repo = temp.path();
    let base_sha = head(repo);
    fs::write(repo.join("README.md"), "reviewer repair\n").unwrap();
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-qm", "reviewer repair"]);
    let reviewer_head = head(repo);
    fs::write(repo.join("README.md"), "resumed implementation\n").unwrap();

    let host = CommitTestHost::new(
        vec![task_with_file("T1", "Candidate", "README.md", "codex")],
        repo.to_path_buf(),
    )
    .with_settled_review("T1", &reviewer_head);
    let output = git_commit(&host, &commit_input(repo, &base_sha))
        .expect("resumed changes on the settled reviewer HEAD must commit");
    assert_eq!(output["decision"], json!("performed"));
    assert_eq!(output["committed"], json!(true));
    assert_eq!(git(repo, &["rev-parse", "HEAD~1"]), reviewer_head);
    assert_eq!(
        git(repo, &["show", "HEAD:README.md"]),
        "resumed implementation"
    );
}

/// HEAD recorded by the settlement but not a descendant of the pin is refused.
#[test]
fn settled_review_head_refused_when_not_descendant_of_pin() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::settled_review_head_refused_when_not_descendant_of_pin"
    )) {
        return;
    }
    let temp = initialized_git_repo();
    let repo = temp.path();
    let base_sha = head(repo);
    git(repo, &["checkout", "--orphan", "foreign-root"]);
    git(repo, &["commit", "--allow-empty", "-qm", "unrelated root"]);
    let foreign_head = head(repo);

    let host = CommitTestHost::new(
        vec![task_with_file("T1", "Candidate", "README.md", "codex")],
        repo.to_path_buf(),
    )
    .with_settled_review("T1", &foreign_head);
    let error = git_commit(&host, &commit_input(repo, &base_sha))
        .expect_err("a non-descendant HEAD must fail closed");
    assert!(
        error.to_string().contains("worktree_head_changed"),
        "unexpected error: {error}"
    );
}

/// Legacy comment settlements may abbreviate a commit, but must not resolve refs.
#[test]
fn legacy_settlement_resolves_short_hash_but_refuses_symbolic_ref() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::legacy_settlement_resolves_short_hash_but_refuses_symbolic_ref"
    )) {
        return;
    }

    let temp = initialized_git_repo();
    let repo = temp.path();
    let base_sha = head(repo);
    git(repo, &["commit", "--allow-empty", "-qm", "reviewer repair"]);
    let reviewer_head = head(repo);
    let short_hash = &reviewer_head[..8];
    let host = CommitTestHost::new(
        vec![task_with_file("T1", "Candidate", "README.md", "codex")],
        repo.to_path_buf(),
    )
    .with_legacy_settled_review("T1", "system", short_hash);
    let output = git_commit(&host, &commit_input(repo, &base_sha))
        .expect("a legacy settlement's short commit id must resolve");
    assert_eq!(output["decision"], json!("already_committed"));

    let temp = initialized_git_repo();
    let repo = temp.path();
    let base_sha = head(repo);
    git(repo, &["commit", "--allow-empty", "-qm", "reviewer repair"]);
    git(
        repo,
        &["commit", "--allow-empty", "-qm", "unrecorded extra"],
    );
    let host = CommitTestHost::new(
        vec![task_with_file("T1", "Candidate", "README.md", "codex")],
        repo.to_path_buf(),
    )
    .with_legacy_settled_review("T1", "system", "HEAD");
    let error = git_commit(&host, &commit_input(repo, &base_sha))
        .expect_err("a ref expression must not authorize a later unrecorded HEAD");
    assert!(
        error.to_string().contains("worktree_head_changed"),
        "unexpected error: {error}"
    );
}

/// A moved HEAD no settled review recorded is refused.
#[test]
fn moved_head_refused_when_not_recorded_by_settled_review() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::moved_head_refused_when_not_recorded_by_settled_review"
    )) {
        return;
    }
    let temp = initialized_git_repo();
    let repo = temp.path();
    let base_sha = head(repo);
    git(
        repo,
        &["commit", "--allow-empty", "-qm", "unrecorded commit"],
    );

    // Settlement exists, but it recorded the base, not the current HEAD.
    let host = CommitTestHost::new(
        vec![task_with_file("T1", "Candidate", "README.md", "codex")],
        repo.to_path_buf(),
    )
    .with_settled_review("T1", &base_sha);
    let error = git_commit(&host, &commit_input(repo, &base_sha))
        .expect_err("an unrecorded HEAD must fail closed");
    assert!(
        error.to_string().contains("worktree_head_changed"),
        "unexpected error: {error}"
    );
}

/// An unrecorded extra commit on top of the settled reviewer commit is refused.
#[test]
fn extra_commit_on_top_of_settled_review_head_is_refused() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::extra_commit_on_top_of_settled_review_head_is_refused"
    )) {
        return;
    }
    let temp = initialized_git_repo();
    let repo = temp.path();
    let base_sha = head(repo);
    git(repo, &["commit", "--allow-empty", "-qm", "reviewer repair"]);
    let reviewer_head = head(repo);
    git(
        repo,
        &["commit", "--allow-empty", "-qm", "unrecorded extra"],
    );

    let host = CommitTestHost::new(
        vec![task_with_file("T1", "Candidate", "README.md", "codex")],
        repo.to_path_buf(),
    )
    .with_settled_review("T1", &reviewer_head);
    let error = git_commit(&host, &commit_input(repo, &base_sha))
        .expect_err("an extra commit above the recorded final commit must fail closed");
    assert!(
        error.to_string().contains("worktree_head_changed"),
        "unexpected error: {error}"
    );
}

/// An implementer comment that copies the legacy settlement format is not a
/// review receipt. The public commit action must refuse the moved HEAD
/// before it stages the dirty tree.
#[test]
fn forged_implementer_settlement_comment_cannot_authorize_moved_head() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::forged_implementer_settlement_comment_cannot_authorize_moved_head"
    )) {
        return;
    }
    let temp = initialized_git_repo();
    let repo = temp.path();
    let base_sha = head(repo);
    git(
        repo,
        &["commit", "--allow-empty", "-qm", "unreviewed descendant"],
    );
    let descendant = head(repo);
    fs::write(repo.join("README.md"), "forged edit\n").unwrap();

    let host = CommitTestHost::new(
        vec![task_with_file("T1", "Candidate", "README.md", "codex")],
        repo.to_path_buf(),
    )
    .with_legacy_settled_review("T1", "codex", &descendant);
    let error = git_commit(&host, &commit_input(repo, &base_sha))
        .expect_err("an implementer's settlement-shaped comment must not admit HEAD");
    assert!(
        error.to_string().contains("worktree_head_changed"),
        "unexpected error: {error}"
    );
    assert_eq!(head(repo), descendant, "refusal must not create a commit");
    assert_eq!(
        git(repo, &["diff", "--cached", "--name-only"]),
        "",
        "refusal must happen before staging"
    );
    assert_eq!(
        fs::read_to_string(repo.join("README.md")).unwrap(),
        "forged edit\n"
    );
    assert_eq!(git(repo, &["show", "HEAD:README.md"]), "base");
}
