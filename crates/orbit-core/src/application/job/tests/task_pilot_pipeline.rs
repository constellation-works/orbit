//! Shipped task-pilot job boundary regressions [ORB-11411].

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_engine::JobOutcome;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::exec::{seed_default_catalogs, try_execute_named_job};
use crate::OrbitRuntime;

struct TaskPilotJobFixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo_root: PathBuf,
    stale_sha: String,
    current_sha: String,
    dirty_status: String,
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
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

fn configure_commits(repo: &Path) {
    git(repo, &["config", "user.name", "Orbit Test"]);
    git(repo, &["config", "user.email", "orbit-test@example.com"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

fn commit_file(repo: &Path, relative: &str, contents: &str) -> String {
    let path = repo.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(&path, contents).expect("write fixture file");
    git(repo, &["add", relative]);
    git(repo, &["commit", "-m", &format!("write {relative}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn task_pilot_job_fixture(config_branch: &str, remote_branch: &str) -> TaskPilotJobFixture {
    let config = format!("[workflow]\nbase_branch = {config_branch:?}\n");
    let root = tempfile::tempdir().expect("create fixture root");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    fs::write(workspace_root.join("config.toml"), config).expect("write workspace config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    seed_default_catalogs(&global_root);

    let remote = root.path().join("remote.git");
    let publisher = root.path().join("publisher");
    let remote_path = remote.to_str().expect("UTF-8 remote path");
    let publisher_path = publisher.to_str().expect("UTF-8 publisher path");

    git(root.path(), &["init", "--bare", remote_path]);
    git(&repo_root, &["init"]);
    git(&repo_root, &["checkout", "-b", remote_branch]);
    configure_commits(&repo_root);
    fs::write(repo_root.join(".gitignore"), ".orbit/\n").expect("ignore workspace state");
    fs::create_dir_all(repo_root.join("src")).expect("create source directory");
    fs::write(repo_root.join("src/base.rs"), "base\n").expect("write base source");
    git(&repo_root, &["add", ".gitignore", "src/base.rs"]);
    git(&repo_root, &["commit", "-m", "seed source"]);
    git(&repo_root, &["remote", "add", "origin", remote_path]);
    git(&repo_root, &["push", "-u", "origin", remote_branch]);
    let stale_sha = git(&repo_root, &["rev-parse", "HEAD"]);

    git(
        root.path(),
        &[
            "clone",
            "--branch",
            remote_branch,
            remote_path,
            publisher_path,
        ],
    );
    configure_commits(&publisher);
    let current_sha = commit_file(&publisher, "src/remote.rs", "remote update\n");
    git(&publisher, &["push", "origin", remote_branch]);

    fs::write(repo_root.join("src/base.rs"), "dirty primary\n").expect("dirty primary source");
    fs::write(repo_root.join("src/untracked.rs"), "untracked\n").expect("write untracked source");
    let dirty_status = git(&repo_root, &["status", "--short"]);

    TaskPilotJobFixture {
        _root: root,
        runtime,
        repo_root,
        stale_sha,
        current_sha,
        dirty_status,
    }
}

fn execute_task_pilot_job(
    fixture: &TaskPilotJobFixture,
    input: Value,
    run_id: &str,
) -> Result<JobOutcome, orbit_engine::DispatchError> {
    try_execute_named_job(
        &fixture.runtime,
        &fixture.repo_root,
        &fixture.runtime,
        "task_pilot_pipeline",
        input,
        run_id,
    )
}

fn assert_job_resolves_branch(config_branch: &str, input: Value, expected_branch: &str) {
    let fixture = task_pilot_job_fixture(config_branch, expected_branch);
    let run_id = format!("task-pilot-{config_branch}-{expected_branch}");
    let outcome = execute_task_pilot_job(&fixture, input, &run_id)
        .expect("shipped task-pilot job must render and reach preparation");

    assert!(outcome.success, "pipeline outcome: {outcome:?}");
    assert_eq!(
        outcome.pipeline["prepare"]["source"]["base_branch"],
        expected_branch
    );
    assert_eq!(
        outcome.pipeline["prepare"]["source"]["source_revision"],
        fixture.current_sha
    );
    assert_eq!(
        git(&fixture.repo_root, &["rev-parse", "HEAD"]),
        fixture.stale_sha,
        "preparation must not advance the primary checkout"
    );
    assert_eq!(
        git(&fixture.repo_root, &["status", "--short"]),
        fixture.dirty_status,
        "preparation must preserve dirty and untracked primary files"
    );
    assert!(!fixture.repo_root.join("src/remote.rs").exists());
}

#[test]
fn shipped_task_pilot_job_renders_omitted_and_empty_workspace_branch_inputs() {
    for (config_branch, input) in [
        ("main", json!({})),
        ("main", json!({ "base_branch": "" })),
        ("agent-main", json!({})),
        ("agent-main", json!({ "base_branch": "" })),
    ] {
        assert_job_resolves_branch(config_branch, input, config_branch);
    }
}

#[test]
fn shipped_task_pilot_job_honors_an_explicit_alternate_branch() {
    let alternate_branch = format!("release-{}", std::process::id());

    assert_job_resolves_branch(
        "main",
        json!({ "base_branch": alternate_branch.clone() }),
        &alternate_branch,
    );
}

#[test]
fn shipped_task_pilot_job_rejects_an_unavailable_explicit_branch() {
    let fixture = task_pilot_job_fixture("main", "main");
    let error = execute_task_pilot_job(
        &fixture,
        json!({ "base_branch": "missing-branch" }),
        "task-pilot-missing-branch",
    )
    .expect_err("an unavailable explicit branch must fail before pilot dispatch");
    let message = error.to_string();

    assert!(message.contains("could not fetch"), "{message}");
    assert!(message.contains("missing-branch"), "{message}");
    assert_eq!(
        git(&fixture.repo_root, &["rev-parse", "HEAD"]),
        fixture.stale_sha
    );
    assert_eq!(
        git(&fixture.repo_root, &["status", "--short"]),
        fixture.dirty_status
    );
}
