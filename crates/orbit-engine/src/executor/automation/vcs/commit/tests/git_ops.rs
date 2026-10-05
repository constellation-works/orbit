#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use orbit_common::security::child_env::AGENT_SUBPROCESS_BASELINE_VARS;
use serde_json::json;

use super::super::actions::commit_batch_changes;
use super::super::git_commit;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};
use crate::executor::automation::vcs::git::git_output;

fn executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make executable");
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
fn commit_child_environment_excludes_parent_secrets() {
    let exact_test = concat!(
        "executor::automation::vcs::commit::tests::git_ops::",
        "commit_child_environment_excludes_parent_secrets"
    );
    if std::env::var("ORBIT_TEST_SECRET").ok().as_deref() == Some(exact_test) {
        let temp = initialized_git_repo();
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
    orbit_common::test_env::assert_child_test_passed(
        exact_test,
        output.status,
        &output.stdout,
        &output.stderr,
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

fn write_rel(repo: &Path, path: &str, contents: &str) {
    let full = repo.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("parent directory");
    }
    fs::write(full, contents).expect("write file");
}

fn commit_names(repo: &Path, revision: &str) -> BTreeSet<String> {
    git_output(repo, &["show", "--format=", "--name-only", revision])
        .expect("commit names")
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// ORB-14096: candidate paths are literal Git pathspecs. A file named `*` or
/// `a[bc]` must stage and commit only itself, not scratch, a character-class
/// neighbour, or another task's files such as `app/i/page.tsx`.
#[test]
fn glob_candidate_names_stage_and_commit_only_themselves() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    write_rel(workspace, "*", "star\n");
    write_rel(workspace, "a[bc]", "brackets\n");
    write_rel(workspace, "ab", "class-neighbour\n");
    write_rel(workspace, "app/[id]/page.tsx", "id page\n");
    write_rel(workspace, "app/i/page.tsx", "other task\n");
    write_rel(workspace, "other.txt", "unrelated\n");
    write_rel(workspace, ".orbit/tmp/scratch.txt", "scratch\n");
    fs::write(workspace.join("README.md"), "tracked modification\n").expect("modify README");

    let glob_task = {
        let mut task = task_with_file("T-GLOB", "Glob names", "unused", "grok");
        task.context_files = vec![
            "file:*".to_string(),
            "file:a[bc]".to_string(),
            "file:app/[id]/page.tsx".to_string(),
        ];
        task
    };
    let other_task = {
        let mut task = task_with_file("T-OTHER", "Other files", "unused", "grok");
        task.context_files = vec![
            "file:ab".to_string(),
            "file:README.md".to_string(),
            "file:app/i/page.tsx".to_string(),
            "file:other.txt".to_string(),
        ];
        task
    };
    let host = CommitTestHost::new(vec![glob_task, other_task], workspace.to_path_buf());
    let output = git_commit(
        &host,
        &json!({
            "scope": "per_task",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
            "completed_task_ids": ["T-GLOB", "T-OTHER"],
        }),
    )
    .expect("literal candidate names commit");

    assert_eq!(output["committed_task_ids"], json!(["T-GLOB", "T-OTHER"]));
    assert_eq!(
        git_output(workspace, &["log", "-1", "--format=%s", "HEAD~1"]).unwrap(),
        "[T-GLOB] Glob names"
    );
    assert_eq!(
        commit_names(workspace, "HEAD~1"),
        BTreeSet::from([
            "*".to_string(),
            "a[bc]".to_string(),
            "app/[id]/page.tsx".to_string(),
        ]),
        "ORB-14096: a candidate named `*` or `a[bc]` must stage and commit only itself"
    );
    assert_eq!(
        commit_names(workspace, "HEAD"),
        BTreeSet::from([
            "README.md".to_string(),
            "ab".to_string(),
            "app/i/page.tsx".to_string(),
            "other.txt".to_string(),
        ]),
        "ORB-14096: another task's files and character-class neighbours stay out of the glob commit"
    );
    assert_eq!(
        git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .unwrap(),
        "?? .orbit/tmp/scratch.txt",
        "ORB-14096: scratch excluded from candidate paths must stay unstaged"
    );
    assert_eq!(
        fs::read_to_string(workspace.join(".orbit/tmp/scratch.txt")).unwrap(),
        "scratch\n"
    );
}
