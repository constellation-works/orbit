#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use orbit_common::security::child_env::AGENT_SUBPROCESS_BASELINE_VARS;
use serde_json::json;

use super::super::commit_batch_changes;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};
use crate::executor::automation::vcs::git::{git_output, git_success};
use crate::executor::automation::vcs::push::push_batch_changes_inner;

fn executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make executable");
}

fn tracked_hooks(repo: &Path) {
    fs::create_dir(repo.join(".hooks")).expect("hooks directory");
    for name in [
        "pre-commit",
        "prepare-commit-msg",
        "post-commit",
        "pre-push",
    ] {
        executable(
            &repo.join(".hooks").join(name),
            &format!("#!/bin/sh\nprintf triggered > .git/{name}-marker\n"),
        );
    }
    git_success(repo, &["add", ".hooks"]).expect("stage hooks");
    git_success(repo, &["commit", "-m", "tracked hooks"]).expect("commit hooks");
    git_success(repo, &["config", "core.hooksPath", ".hooks"]).expect("configure tracked hooks");
}

fn commit_candidate(repo: &Path) {
    fs::write(repo.join("README.md"), "candidate\n").expect("candidate change");
    let host = CommitTestHost::new(
        vec![task_with_file("T1", "Candidate", "README.md", "codex")],
        repo.to_path_buf(),
    );
    commit_batch_changes(
        &host,
        &json!({"workspace_path": repo, "job_run_id": "batch-1"}),
    )
    .expect("commit candidate");
    assert_eq!(
        git_output(repo, &["show", "HEAD:README.md"]).unwrap(),
        "candidate"
    );
}

#[test]
fn commit_batch_disables_tracked_repository_hooks() {
    let temp = initialized_git_repo();
    let repo = temp.path();
    tracked_hooks(repo);
    commit_candidate(repo);
    for name in ["pre-commit", "prepare-commit-msg", "post-commit"] {
        assert!(
            !repo.join(format!(".git/{name}-marker")).exists(),
            "{name} ran"
        );
    }

    // Positive control: the same executable hook runs under ordinary Git.
    let control = Command::new("git")
        .current_dir(repo)
        .args(["commit", "--allow-empty", "-m", "control"])
        .output()
        .expect("control commit");
    assert!(control.status.success());
    assert!(repo.join(".git/pre-commit-marker").exists());
}

#[test]
fn push_batch_disables_tracked_repository_hooks() {
    let temp = initialized_git_repo();
    let repo = temp.path();
    tracked_hooks(repo);
    let remote = tempfile::tempdir().expect("remote");
    git_success(remote.path(), &["init", "--bare"]).expect("bare remote");
    git_success(
        repo,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    )
    .expect("configure remote");
    let branch = git_output(repo, &["branch", "--show-current"]).unwrap();
    let host = CommitTestHost::new(Vec::new(), repo.to_path_buf());
    let result = push_batch_changes_inner(&host, &json!({"branch": branch}), repo)
        .expect("push candidate through real private operation");
    assert_eq!(result["decision"], "performed_create");
    assert_eq!(
        git_output(
            remote.path(),
            &["rev-parse", &format!("refs/heads/{branch}")]
        )
        .unwrap(),
        git_output(repo, &["rev-parse", "HEAD"]).unwrap()
    );
    assert!(!repo.join(".git/pre-push-marker").exists());

    let control = Command::new("git")
        .current_dir(repo)
        .args(["push", "origin", &branch])
        .output()
        .expect("control push");
    assert!(control.status.success());
    assert!(repo.join(".git/pre-push-marker").exists());
}

/// [ORB-12103] A self-pointing `origin` makes `ls-remote` echo the local branch
/// back, which previously produced `decision: reused_current` and a green push
/// step for a branch that was never published anywhere.
#[test]
fn push_refuses_an_origin_that_is_this_same_repository() {
    let temp = initialized_git_repo();
    let repo = temp.path();
    git_success(repo, &["remote", "add", "origin", repo.to_str().unwrap()])
        .expect("self-pointing remote");
    let branch = git_output(repo, &["branch", "--show-current"]).unwrap();
    let host = CommitTestHost::new(Vec::new(), repo.to_path_buf());

    let error = push_batch_changes_inner(&host, &json!({"branch": branch}), repo)
        .expect_err("a push into this same repository must not report success");

    let message = error.to_string();
    assert!(
        message.contains("is this same repository") && message.contains(&branch),
        "denial must explain that nothing was published, got: {message}"
    );
}

/// The observed shape of the fault: a linked worktree shares `.git/config` with
/// its primary checkout, so an `origin` naming that checkout is the worktree's
/// own repository too.
#[test]
fn push_refuses_an_origin_naming_the_primary_checkout_of_this_worktree() {
    let temp = initialized_git_repo();
    let repo = temp.path();
    git_success(repo, &["remote", "add", "origin", repo.to_str().unwrap()])
        .expect("self-pointing remote");
    let worktree = temp.path().join("linked-worktree");
    git_success(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            "task-branch",
            worktree.to_str().unwrap(),
        ],
    )
    .expect("linked worktree");
    let host = CommitTestHost::new(Vec::new(), worktree.clone());

    let error = push_batch_changes_inner(&host, &json!({"branch": "task-branch"}), &worktree)
        .expect_err("a push into the primary checkout must not report success");

    assert!(
        error.to_string().contains("is this same repository"),
        "denial must identify the shared repository, got: {error}"
    );
}

/// A remote naming a different repository on disk stays a real publication
/// target, so the guard must not turn local-remote fixtures into failures.
#[test]
fn push_accepts_a_distinct_local_repository_as_origin() {
    let temp = initialized_git_repo();
    let repo = temp.path();
    let remote = tempfile::tempdir().expect("remote");
    git_success(remote.path(), &["init", "--bare"]).expect("bare remote");
    git_success(
        repo,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    )
    .expect("configure remote");
    let branch = git_output(repo, &["branch", "--show-current"]).unwrap();
    let host = CommitTestHost::new(Vec::new(), repo.to_path_buf());

    let result = push_batch_changes_inner(&host, &json!({"branch": branch}), repo)
        .expect("push to a distinct repository");

    assert_eq!(result["decision"], "performed_create");
}

#[test]
fn commit_child_environment_excludes_parent_secrets() {
    let exact_test = concat!(
        "executor::automation::vcs::commit::tests::git_ops::",
        "commit_child_environment_excludes_parent_secrets"
    );
    if std::env::var("ORBIT_TEST_SECRET").ok().as_deref() == Some(exact_test) {
        let temp = initialized_git_repo();
        tracked_hooks(temp.path());
        commit_candidate(temp.path());
        return;
    }

    // Observe Git's entry environment rather than enabling a repository hook.
    // Isolating PATH and the canary in a child avoids process-global env races.
    let bin = tempfile::tempdir().expect("observer directory");
    let observed_path = bin.path().join("observed-env");
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate Git");
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout).expect("Git path UTF-8");
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    executable(
        &bin.path().join("git"),
        &format!(
            "#!/bin/sh\nif [ -n \"$GIT_AUTHOR_NAME\" ]; then env -0 > {}; fi\nexec {} \"$@\"\n",
            quote(observed_path.to_str().unwrap()),
            quote(real_git.trim()),
        ),
    );
    let mut paths = vec![bin.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", exact_test, "--nocapture"])
        .env("ORBIT_TEST_SECRET", exact_test)
        .env("PATH", std::env::join_paths(paths).expect("observer PATH"))
        .output()
        .expect("isolated environment test");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let observed = fs::read(observed_path).expect("production commit ran the observer");
    let observed = String::from_utf8(observed).expect("environment UTF-8");
    assert!(!observed.contains("ORBIT_TEST_SECRET="));
    assert!(observed.contains("GIT_AUTHOR_NAME="));
    assert!(observed.contains("GIT_COMMITTER_NAME="));
    assert!(observed.contains("GIT_OPTIONAL_LOCKS=0"));
    for entry in observed.split('\0').filter(|entry| !entry.is_empty()) {
        let name = entry.split_once('=').expect("environment entry").0;
        // The observer shell adds PWD, SHLVL and _ after process creation.
        let git_context = matches!(
            name,
            "SSH_AUTH_SOCK"
                | "GIT_OPTIONAL_LOCKS"
                | "GIT_AUTHOR_NAME"
                | "GIT_AUTHOR_EMAIL"
                | "GIT_COMMITTER_NAME"
                | "GIT_COMMITTER_EMAIL"
                | "PWD"
                | "SHLVL"
                | "_"
        );
        assert!(
            AGENT_SUBPROCESS_BASELINE_VARS.contains(&name) || git_context,
            "unexpected commit child variable: {name}"
        );
    }
}
