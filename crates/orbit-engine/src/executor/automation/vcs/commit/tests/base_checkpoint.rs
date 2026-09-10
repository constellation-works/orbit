//! ORB-10380: the commit step compares the task branch HEAD with the commit
//! `worktree_setup` pinned, never against a ref name.
//!
//! `refs/remotes/origin/<base>` is shared by every worktree hanging off one
//! `.git`. A sibling run's setup fetch, a rescue fetch, or a merge moves it
//! while other runs are still in flight, so a commit step that re-resolved the
//! name failed every older run by construction. These tests pin the immutable
//! base contract, the ADR-0219 carve-out reachability, and the rule that no
//! failure path mutates the worktree on its way out.

use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::NO_DIFF_EXPECTED_TAG;
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::git_commit;
use super::test_support::*;

use super::super::super::git::{git_output, git_success};

const MOVING_BASE_REF: &str = "origin/agent-main";
const HANDOFF_RUN_ID: &str = "batch-1";
const RESUME_RUN_ID: &str = "resume-1";

fn commit_all(workspace: &Path, message: &str) -> String {
    git_success(workspace, &["add", "--all", "--", "."]).expect("stage fixture change");
    git_success(workspace, &["commit", "-m", message]).expect("commit fixture change");
    git_output(workspace, &["rev-parse", "HEAD"]).expect("read fixture head")
}

/// Point the shared remote-tracking ref at `sha`, the way a fetch or a merge in
/// a sibling worktree does.
fn move_shared_base_ref(workspace: &Path, sha: &str) {
    git_success(
        workspace,
        &[
            "update-ref",
            &format!("refs/remotes/{MOVING_BASE_REF}"),
            sha,
        ],
    )
    .expect("move shared base ref");
}

fn batch_input(workspace: &Path, base_sha: &str) -> Value {
    json!({
        "scope": "all",
        "job_run_id": "batch-1",
        "workspace_path": workspace.to_string_lossy().to_string(),
        "base_ref": MOVING_BASE_REF,
        "base_sha": base_sha,
    })
}

fn preserved_run_state(run_id: &str, base_sha: &str, head_sha: &str) -> PipelineState {
    let mut state = PipelineState::new(
        run_id.to_string(),
        "task_pr_pipeline".to_string(),
        json!({"task_ids": ["T1"]}),
    );
    state.record_failure_activity(
        "pr_failure_handoff".to_string(),
        "implement_bundle".to_string(),
        json!({
            "phase": "failure_handoff",
            "decision": "blocked_failure_pr",
            "task_id": "T1",
            "handoff_run_id": HANDOFF_RUN_ID,
            "checkpoint_owner": HANDOFF_RUN_ID,
            "preservation_commit_created": true,
            "head_sha": head_sha,
            "original_base_sha": base_sha,
        }),
    );
    state
}

fn host_with_preservation(
    workspace: &Path,
    base_sha: &str,
    preserved_head: &str,
) -> CommitTestHost {
    let task = task_with_file("T1", "Preserved resume", "task.txt", "claude");
    let source = preserved_run_state(HANDOFF_RUN_ID, base_sha, preserved_head);
    let mut resumed = source.clone();
    resumed.run_id = RESUME_RUN_ID.to_string();
    CommitTestHost::new(vec![task], workspace.to_path_buf())
        .with_run_state(HANDOFF_RUN_ID, None, source)
        .with_run_state(RESUME_RUN_ID, Some(HANDOFF_RUN_ID), resumed)
}

#[test]
fn commit_survives_the_shared_base_ref_moving_after_worktree_setup() {
    // The regression that matters: a sibling run advances `origin/agent-main`
    // mid-run. Before ORB-10380 the commit step re-resolved that name, found the
    // new tip was not an ancestor of HEAD, and failed the whole run.
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read setup checkpoint");
    move_shared_base_ref(workspace, &base_sha);
    git_success(workspace, &["checkout", "-b", "orbit/T1"]).expect("create task branch");

    // A sibling run's `worktree_setup` fetch lands a newer base.
    git_success(workspace, &["checkout", "--detach", &base_sha]).expect("detach at base");
    fs::write(workspace.join("sibling.txt"), "sibling run work\n").unwrap();
    let advanced_base = commit_all(workspace, "sibling run merged first");
    move_shared_base_ref(workspace, &advanced_base);
    git_success(workspace, &["checkout", "orbit/T1"]).expect("return to the task branch");
    fs::write(workspace.join("task.txt"), "task work\n").unwrap();
    git_success(workspace, &["add", "--", "task.txt"]).unwrap();
    assert_ne!(
        git_output(workspace, &["rev-parse", MOVING_BASE_REF]).expect("read moved base"),
        base_sha,
        "precondition: the shared base ref moved while the run was in flight"
    );

    let task = task_with_file("T1", "Pinned base task", "task.txt", "claude");
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());

    let result = git_commit(&host, &batch_input(workspace, &base_sha))
        .expect("a moved shared base ref must not fail the commit step");

    assert_eq!(result["decision"], "performed");
    assert_eq!(result["base_sha"], base_sha);
    let task_head = result["commit_sha"].as_str().expect("workflow commit SHA");
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("read final head"),
        task_head,
        "the pipeline returns the commit it created"
    );
}

#[test]
fn commit_rejects_a_base_sha_input_that_is_a_ref_name() {
    // The contract is a pinned commit id. Accepting a name here would quietly
    // restore the moving-base failure.
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");
    move_shared_base_ref(workspace, &base_sha);

    let task = task_with_file("T1", "Pinned base task", "task.txt", "claude");
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());

    let error = git_commit(&host, &batch_input(workspace, MOVING_BASE_REF))
        .expect_err("a ref name is not a pinned base");

    let OrbitError::InvalidInput(message) = error else {
        panic!("expected invalid input");
    };
    assert!(
        message.contains("must be the full commit id pinned by worktree_setup"),
        "{message}"
    );
}

#[test]
fn commit_rejects_any_head_change_from_the_pinned_base() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");

    git_success(workspace, &["checkout", "--orphan", "unrelated"])
        .expect("start unrelated history");
    git_success(workspace, &["rm", "-rf", "--cached", "."]).expect("clear orphan index");
    fs::write(workspace.join("unrelated.txt"), "unrelated root\n").unwrap();
    let unrelated_head = commit_all(workspace, "unrelated root commit");

    let task = task_with_file("T1", "Unrelated history", "unrelated.txt", "claude");
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());

    let error =
        git_commit(&host, &batch_input(workspace, &base_sha)).expect_err("changed HEAD fails");

    let message = error.to_string();
    assert!(message.contains("worktree_head_changed"), "{message}");
    assert!(
        message.contains(&base_sha),
        "names the pinned base: {message}"
    );
    assert!(message.contains(&unrelated_head), "names HEAD: {message}");
    assert!(
        !message.contains("nothing to commit"),
        "the immutable-base failure must not reuse the empty-stage wording: {message}"
    );
}

#[test]
fn commit_accepts_only_the_exact_orbit_preservation_head_for_a_resume() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");
    git_success(workspace, &["checkout", "-b", "orbit/T1"]).expect("create task branch");
    fs::write(workspace.join("candidate.txt"), "preserved candidate\n").unwrap();
    let preserved_head = commit_all(workspace, "[T1] Orbit failure preservation");
    fs::write(workspace.join("task.txt"), "resumed edit\n").unwrap();
    git_success(workspace, &["add", "--", "task.txt"]).unwrap();

    let host = host_with_preservation(workspace, &base_sha, &preserved_head);
    let mut input = batch_input(workspace, &base_sha);
    input["run_id"] = json!(RESUME_RUN_ID);

    let result = git_commit(&host, &input).expect("known preservation head is accepted");

    assert_eq!(result["decision"], "performed");
    assert_eq!(result["base_sha"], base_sha);
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD^1"]).expect("read commit parent"),
        preserved_head,
        "the resumed workflow commit extends the preservation commit",
    );
    let source = host
        .read_run_state(HANDOFF_RUN_ID)
        .expect("read source state")
        .expect("source state exists");
    assert_eq!(source.run_id, HANDOFF_RUN_ID);
    assert_eq!(
        source
            .failure_activity_checkpoint
            .expect("preservation evidence")
            .output["original_base_sha"],
        base_sha,
        "the original base evidence remains unchanged",
    );
}

#[test]
fn commit_rejects_a_commit_after_the_recorded_preservation_head_without_mutation() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");
    git_success(workspace, &["checkout", "-b", "orbit/T1"]).expect("create task branch");
    fs::write(workspace.join("candidate.txt"), "preserved candidate\n").unwrap();
    let preserved_head = commit_all(workspace, "[T1] Orbit failure preservation");
    fs::write(
        workspace.join("unknown.txt"),
        "unknown committed movement\n",
    )
    .unwrap();
    let unknown_head = commit_all(workspace, "unknown commit");
    fs::write(workspace.join("task.txt"), "user work must survive\n").unwrap();
    let status_before = git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .expect("status before refusal");

    let host = host_with_preservation(workspace, &base_sha, &preserved_head);
    let mut input = batch_input(workspace, &base_sha);
    input["run_id"] = json!(RESUME_RUN_ID);
    let error = git_commit(&host, &input).expect_err("unknown movement remains rejected");

    assert!(error.to_string().contains("expected base"), "{error}");
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("head after refusal"),
        unknown_head,
    );
    assert_eq!(
        git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .expect("status after refusal"),
        status_before,
        "candidate and user changes are untouched",
    );
}

#[test]
fn preservation_evidence_cannot_launder_an_unknown_parent_commit() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");
    git_success(workspace, &["checkout", "-b", "orbit/T1"]).expect("create task branch");
    fs::write(
        workspace.join("unknown.txt"),
        "unknown committed movement\n",
    )
    .unwrap();
    let unknown_head = commit_all(workspace, "unknown commit");
    fs::write(workspace.join("candidate.txt"), "claimed preservation\n").unwrap();
    let claimed_preservation = commit_all(workspace, "Orbit preservation");
    fs::write(workspace.join("task.txt"), "user work must survive\n").unwrap();
    let status_before = git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .expect("status before refusal");

    let host = host_with_preservation(workspace, &base_sha, &claimed_preservation);
    let mut input = batch_input(workspace, &base_sha);
    input["run_id"] = json!(RESUME_RUN_ID);
    let error = git_commit(&host, &input).expect_err("unknown parent remains rejected");

    let message = error.to_string();
    assert!(message.contains("unowned parent"), "{message}");
    assert!(message.contains(&unknown_head), "{message}");
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("head after refusal"),
        claimed_preservation,
    );
    assert_eq!(
        git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .expect("status after refusal"),
        status_before,
    );
}

#[test]
fn unrelated_history_failure_leaves_the_worktree_exactly_as_found() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");

    git_success(workspace, &["checkout", "--orphan", "unrelated"])
        .expect("start unrelated history");
    git_success(workspace, &["rm", "-rf", "--cached", "."]).expect("clear orphan index");
    fs::write(workspace.join("unrelated.txt"), "unrelated root\n").unwrap();
    let head_before = commit_all(workspace, "unrelated root commit");

    // Leave the checkout dirty in both directions: one staged change, one
    // untracked file. A failure path must touch neither.
    fs::write(workspace.join("unrelated.txt"), "edited in place\n").unwrap();
    git_success(workspace, &["add", "unrelated.txt"]).expect("stage an edit");
    fs::write(workspace.join("scratch.txt"), "untracked scratch\n").unwrap();
    let index_before = git_stdout_bytes(
        workspace,
        &["diff", "--cached", "--binary", "HEAD", "--"],
        "snapshot index before",
    );
    let status_before = git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .expect("snapshot status before");

    let task = task_with_file("T1", "Unrelated history", "unrelated.txt", "claude");
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());

    git_commit(&host, &batch_input(workspace, &base_sha)).expect_err("the run fails");

    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("read head after"),
        head_before,
        "no commit may be created on a failure path"
    );
    assert_eq!(
        git_stdout_bytes(
            workspace,
            &["diff", "--cached", "--binary", "HEAD", "--"],
            "snapshot index after",
        ),
        index_before,
        "the index must be left as found"
    );
    assert_eq!(
        git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .expect("snapshot status after"),
        status_before,
        "the worktree must be left as found"
    );
}

#[test]
fn empty_stage_failure_leaves_the_index_as_found() {
    // ORB-10380: the old empty-diff branch ran `git reset HEAD` on its way out.
    // A failure path never mutates the checkout it is reporting on.
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");
    let index_before = git_stdout_bytes(
        workspace,
        &["diff", "--cached", "--binary", "HEAD", "--"],
        "snapshot index before",
    );

    let task = task_with_file("T1", "Empty task", "src/missing.txt", "claude");
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());

    git_commit(&host, &batch_input(workspace, &base_sha)).expect_err("empty stage errors");

    assert_eq!(
        git_stdout_bytes(
            workspace,
            &["diff", "--cached", "--binary", "HEAD", "--"],
            "snapshot index after",
        ),
        index_before
    );
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("read head after"),
        base_sha
    );
}

#[test]
fn no_diff_expected_task_skips_the_phase_even_when_its_base_is_unreachable() {
    // ADR-0219's carve-out applies before a changed-HEAD failure so a
    // side-effect-only task remains skippable without Git reconciliation.
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");

    git_success(workspace, &["checkout", "--orphan", "unrelated"])
        .expect("start unrelated history");
    git_success(workspace, &["rm", "-rf", "--cached", "."]).expect("clear orphan index");
    fs::write(workspace.join("unrelated.txt"), "unrelated root\n").unwrap();
    let head_before = commit_all(workspace, "unrelated root commit");

    let mut task = task_with_file("T1", "QA validation", "src/missing.txt", "sonnet");
    task.tags.push(NO_DIFF_EXPECTED_TAG.to_string());
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());

    let result = git_commit(&host, &batch_input(workspace, &base_sha))
        .expect("a side-effect-only task skips the phase");

    assert_eq!(result["skipped_no_diff_expected"], json!(true));
    assert_eq!(result["decision"], "skipped_no_diff_expected");
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("read head after"),
        head_before,
        "the skip creates no commit"
    );
}

#[test]
fn allow_empty_skips_a_clean_stage_without_the_no_diff_tag() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");
    git_success(workspace, &["checkout", "-b", "orbit/T1"]).expect("create task branch");

    let task = task_with_file("T1", "Empty epic", "src/missing.txt", "claude");
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());
    let mut input = batch_input(workspace, &base_sha);
    input["allow_empty"] = json!(true);

    let result = git_commit(&host, &input).expect("allow_empty skips a clean stage");
    assert_eq!(result["skipped_no_diff_expected"], json!(true));
    assert_eq!(result["decision"], "skipped_no_diff_expected");
}

#[test]
fn allow_moved_head_reports_already_committed_when_children_advanced_head() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");
    git_success(workspace, &["checkout", "-b", "epic/ORB-EPIC"]).expect("create epic branch");
    fs::write(workspace.join("child.txt"), "landed child\n").unwrap();
    let head_after_child = commit_all(workspace, "child landed into epic");

    let task = task_with_file("T1", "Epic root", "child.txt", "claude");
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());
    let mut input = batch_input(workspace, &base_sha);
    input["allow_empty"] = json!(true);
    input["allow_moved_head"] = json!(true);

    let result = git_commit(&host, &input).expect("moved HEAD with no leftover work");
    assert_eq!(result["skipped_no_diff_expected"], json!(false));
    assert_eq!(result["decision"], "already_committed");
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("read head after"),
        head_after_child
    );
}

#[test]
fn allow_moved_head_commits_leftover_finisher_work() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base_sha = git_output(workspace, &["rev-parse", "HEAD"]).expect("read checkpoint");
    git_success(workspace, &["checkout", "-b", "epic/ORB-EPIC"]).expect("create epic branch");
    fs::write(workspace.join("child.txt"), "landed child\n").unwrap();
    commit_all(workspace, "child landed into epic");
    fs::write(workspace.join("finisher.txt"), "leftover finisher work\n").unwrap();
    git_success(workspace, &["add", "--", "finisher.txt"]).unwrap();

    let task = task_with_file("T1", "Epic root", "finisher.txt", "claude");
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());
    let mut input = batch_input(workspace, &base_sha);
    input["allow_empty"] = json!(true);
    input["allow_moved_head"] = json!(true);

    let result = git_commit(&host, &input).expect("leftover finisher work is committed");
    assert_eq!(result["committed"], json!(true));
    assert_eq!(result["skipped_no_diff_expected"], json!(false));
    assert!(
        workspace.join("finisher.txt").exists(),
        "finisher file remains after commit"
    );
}
