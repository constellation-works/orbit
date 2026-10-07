//! A PR-mode workspace whose Git remotes name no network host is refused at
//! admission — explicit ship, the drain's backlog and readiness, and
//! task-pilot preparation — before any run, worktree or crew exists. A
//! `delivery:task_local_pipeline` tag delivers that one task locally instead.

use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, TaskComplexity, TaskStatus,
    WorkspaceRuntimeBinding,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

const LOCAL_TAG: &str = "delivery:task_local_pipeline";

/// A workspace registered for PR delivery whose only remote, `origin`, is a
/// local bare repository — the shape a git server moved onto the same box
/// leaves behind.
struct Forgeless {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    bare: PathBuf,
}

fn open() -> Forgeless {
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let bare = root.path().join("constellation.git");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "seed"]);
    git(root.path(), &["init", "--bare", bare.to_str().unwrap()]);
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&repo, &["push", "origin", "main"]);
    let binding = WorkspaceRuntimeBinding {
        logical_workspace_id: "ws_forgeless".to_string(),
        task_partition_id: "ws_forgeless".to_string(),
        owner_machine_id: None,
        checkout_role: None,
        repo_root: repo.clone(),
        ship_mode: ShipMode::Pr,
        base_branch: Some("main".to_string()),
    };
    let runtime =
        OrbitRuntime::from_roots_with_binding(&global, &repo.join(".orbit"), binding).unwrap();
    Forgeless {
        _root: root,
        runtime,
        repo,
        bare,
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(args).current_dir(cwd).output().unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn seed(runtime: &OrbitRuntime, title: &str, tags: &[&str]) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The task is observable.".to_string()],
            plan: "Fixture plan.".to_string(),
            tags: tags.iter().map(ToString::to_string).collect(),
            complexity: TaskComplexity::Medium,
            context_files: vec!["file:README.md".into()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed task")
        .id
        .to_string()
}

/// An explicit PR ship of `task`. The fixture deploys no job asset, so a
/// task admission lets through fails next, on the missing asset.
fn ship(runtime: &OrbitRuntime, task: &str) -> OrbitError {
    runtime
        .submit_ship_run(
            ShipMode::Pr,
            Some("main"),
            &[task.to_string()],
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
            JobRunTrigger::cli(),
        )
        .expect_err("a fixture without job assets never submits a run")
}

fn action(runtime: &OrbitRuntime, action: &str, input: Value) -> Value {
    runtime
        .run_deterministic(action, &json!({}), &input, ToolContext::default())
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

fn excluded<'a>(output: &'a Value, field: &str, task: &str) -> Option<&'a Value> {
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry[field] == task)
}

fn readiness_entry(runtime: &OrbitRuntime, task: &str) -> Value {
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .cloned()
        .unwrap_or_else(|| panic!("{task} missing from readiness: {readiness}"))
}

/// The refusal names the remote it found and both ways out.
fn assert_names_remote_and_fixes(message: &str, bare: &Path) {
    for expected in [
        bare.to_str().unwrap(),
        "orbit workspace ship-mode local",
        LOCAL_TAG,
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
}

/// Explicit ship, the drain's backlog and readiness refuse an untagged task
/// with the typed refusal before any run or worktree exists, and admit the
/// tagged one. A remote on a network host lifts the refusal.
#[test]
fn a_pr_workspace_without_a_forge_remote_refuses_untagged_work_before_any_run() {
    if !isolated(
        "pr_forge_admission::a_pr_workspace_without_a_forge_remote_refuses_untagged_work_before_any_run",
    ) {
        return;
    }
    let fixture = open();
    let runtime = &fixture.runtime;
    let plain = seed(runtime, "plain", &[]);
    let local = seed(runtime, "local", &[LOCAL_TAG]);

    let refused = ship(runtime, &plain);
    assert!(
        matches!(refused, OrbitError::PrForgeRemoteMissing { .. }),
        "{refused:?}"
    );
    assert_names_remote_and_fixes(&refused.to_string(), &fixture.bare);
    let admitted = ship(runtime, &local);
    assert!(
        matches!(admitted, OrbitError::NotFound { .. }),
        "a tagged task passes admission and reaches the job load: {admitted:?}"
    );
    let runs = runtime.list_job_runs(JobRunListParams::default()).unwrap();
    assert!(runs.is_empty(), "no run may exist: {runs:#?}");
    let worktrees = git(&fixture.repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        worktrees.matches("worktree ").count(),
        1,
        "no worktree may exist: {worktrees}"
    );

    let backlog = action(runtime, "list_backlog_tasks", json!({}));
    assert_eq!(backlog["task_ids"], json!([local]), "{backlog}");
    let held = excluded(&backlog, "id", &plain).expect("the plain task is withheld");
    assert_eq!(held["reason"], "pr_forge_remote_missing", "{backlog}");
    assert_names_remote_and_fixes(held["detail"].as_str().unwrap(), &fixture.bare);
    let entry = readiness_entry(runtime, &plain);
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "pr_forge_remote_missing", "{entry}");
    assert_eq!(readiness_entry(runtime, &local)["eligible"], true);

    // Any network host is a forge candidate: `gh` alone knows enterprise
    // hosts and SSH aliases, so admission only refuses a set that has none.
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "upstream",
            "https://github.com/example/forgeless.git",
        ],
    );
    let backlog = action(runtime, "list_backlog_tasks", json!({}));
    assert_eq!(backlog["task_ids"], json!([plain, local]), "{backlog}");
}

/// Task-pilot preparation does not stage work the PR pipeline could never
/// open a pull request for, and reports the refusal once.
#[test]
fn pilot_preparation_skips_work_a_forgeless_pr_workspace_cannot_deliver() {
    if !isolated(
        "pr_forge_admission::pilot_preparation_skips_work_a_forgeless_pr_workspace_cannot_deliver",
    ) {
        return;
    }
    let fixture = open();
    let runtime = &fixture.runtime;
    let plain = seed(runtime, "plain", &[]);
    let local = seed(runtime, "local", &[LOCAL_TAG]);

    let prepared = action(
        runtime,
        "prepare_task_pilot",
        json!({
            "task_ids": [plain, local],
            "workspace_path": fixture.repo,
            "base_branch": "main",
        }),
    );
    assert_eq!(prepared["task_ids"], json!([local]), "{prepared}");
    let skipped = excluded(&prepared, "task_id", &plain).expect("the plain task is skipped");
    assert_eq!(skipped["reason"], "pr_forge_remote_missing", "{prepared}");
    assert_names_remote_and_fixes(
        prepared["pr_forge_refusal"].as_str().unwrap(),
        &fixture.bare,
    );
}
